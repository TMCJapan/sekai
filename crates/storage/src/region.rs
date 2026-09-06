//! Derived per-region fingerprints for incremental snapshots.
//!
//! Rationale: `region_state` maps each region file to the `(mtime, size,
//! header hash)` observed when it was last ingested. A file matching all
//! three signals skips read/hash/CAS entirely; its history rows are carried
//! into the new snapshot with one `INSERT ... SELECT` per region instead of
//! a per-chunk Rust loop. The table is purely derived: wiping it degrades
//! the next backup to a full ingest (every fingerprint misses), never to
//! wrong data, and the following run is incremental again.

use rusqlite::{Connection, params};
use sekai_core::{Dimension, RegionKind, SnapshotId};

use crate::error::StorageError;

/// Identity of one region file within its namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RegionKey {
    /// Dimension namespace.
    pub dim: Dimension,
    /// Region family.
    pub kind: RegionKind,
    /// Region X from the file name.
    pub rx: i32,
    /// Region Z from the file name.
    pub rz: i32,
}

impl RegionKey {
    /// Construct a region identity.
    #[must_use]
    pub const fn new(dim: Dimension, kind: RegionKind, rx: i32, rz: i32) -> Self {
        Self { dim, kind, rx, rz }
    }
}

/// Freshly observed file fingerprint, staged for `region_state` upsert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionFingerprint {
    /// Which file this fingerprint describes.
    pub key: RegionKey,
    /// Last modification time as Unix millis (`None` when unavailable).
    pub mtime_ms: Option<u64>,
    /// File size in bytes.
    pub size: u64,
    /// Blake3 of the file's location-table sector (first 4 KiB).
    pub header_hash: [u8; 32],
}

/// Stored fingerprint of the snapshot that last ingested the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionStateEntry {
    /// Which file this state describes.
    pub key: RegionKey,
    /// Last modification time as Unix millis (`None` when unavailable).
    pub mtime_ms: Option<u64>,
    /// File size in bytes.
    pub size: u64,
    /// Blake3 of the file's location-table sector (first 4 KiB).
    pub header_hash: [u8; 32],
    /// Snapshot that last confirmed this fingerprint (ingested, or carried
    /// over by a fingerprint match).
    pub snapshot_id: SnapshotId,
}

impl RegionFingerprint {
    /// Whether the file can skip ingestion against stored state.
    ///
    /// All three signals must agree, and a missing `mtime` on either side
    /// forces ingest: silently trusting a clock the platform cannot provide
    /// would risk stale snapshots, so the fail-safe direction is to redo
    /// the work. A same-size in-place payload rewrite that keeps the
    /// location table unchanged inside one `mtime` granularity tick still
    /// slips through; filesystems with coarse timestamps (e.g. FAT with 2 s
    /// granularity) widen that window, so callers must quiesce the server
    /// before snapshotting.
    #[must_use]
    pub fn matches_state(&self, state: &RegionStateEntry) -> bool {
        if self.key != state.key {
            return false;
        }
        if self.mtime_ms != state.mtime_ms || self.mtime_ms.is_none() {
            return false;
        }
        self.size == state.size && self.header_hash == state.header_hash
    }
}

/// Load every stored region fingerprint.
pub fn load_region_states(conn: &Connection) -> Result<Vec<RegionStateEntry>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT dim, kind, rx, rz, mtime_ms, size, header_hash, snapshot_id
         FROM region_state ORDER BY dim, kind, rx, rz",
    )?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let dim: i64 = row.get(0)?;
        let kind: i64 = row.get(1)?;
        let rx: i64 = row.get(2)?;
        let rz: i64 = row.get(3)?;
        let mtime_ms: Option<i64> = row.get(4)?;
        let size: i64 = row.get(5)?;
        let header_hash: Vec<u8> = row.get(6)?;
        let snapshot_id: i64 = row.get(7)?;
        let invalid =
            |column: &'static str, value: i64| StorageError::InvalidHistoryValue { column, value };
        let header_hash = <[u8; 32]>::try_from(header_hash)
            .map_err(|left| StorageError::InvalidHashLength { len: left.len() })?;
        out.push(RegionStateEntry {
            key: RegionKey::new(
                Dimension(i32::try_from(dim).map_err(|_| invalid("dim", dim))?),
                RegionKind(u8::try_from(kind).map_err(|_| invalid("kind", kind))?),
                i32::try_from(rx).map_err(|_| invalid("rx", rx))?,
                i32::try_from(rz).map_err(|_| invalid("rz", rz))?,
            ),
            mtime_ms: mtime_ms
                .map(|ms| u64::try_from(ms).map_err(|_| invalid("mtime_ms", ms)))
                .transpose()?,
            size: u64::try_from(size).map_err(|_| invalid("size", size))?,
            header_hash,
            snapshot_id: SnapshotId(snapshot_id.cast_unsigned()),
        });
    }
    Ok(out)
}

/// Upsert freshly ingested fingerprints under `snapshot`.
pub fn insert_region_states(
    conn: &Connection,
    snapshot: SnapshotId,
    fingerprints: &[RegionFingerprint],
) -> Result<(), StorageError> {
    if fingerprints.is_empty() {
        return Ok(());
    }
    let mut stmt = conn.prepare(
        "INSERT INTO region_state
         (dim, kind, rx, rz, mtime_ms, size, header_hash, snapshot_id)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(dim, kind, rx, rz) DO UPDATE SET
           mtime_ms = excluded.mtime_ms,
           size = excluded.size,
           header_hash = excluded.header_hash,
           snapshot_id = excluded.snapshot_id",
    )?;
    for fp in fingerprints {
        stmt.execute(params![
            i64::from(fp.key.dim.raw()),
            i64::from(fp.key.kind.raw()),
            i64::from(fp.key.rx),
            i64::from(fp.key.rz),
            fp.mtime_ms.map(|ms| i64::try_from(ms).unwrap_or(i64::MAX)),
            i64::try_from(fp.size).unwrap_or(i64::MAX),
            fp.header_hash.as_slice(),
            snapshot.0.cast_signed(),
        ])?;
    }
    Ok(())
}

/// Retarget carried regions to the new snapshot without touching fingerprints.
///
/// Carried files were fingerprint-matched, so their `(mtime, size,
/// header_hash)` are unchanged; only `snapshot_id` advances. Keeps the
/// column meaning "last snapshot this file was confirmed present" and keeps
/// `REFERENCES snapshots(id)` viable for future snapshot pruning.
pub fn retarget_region_states(
    conn: &Connection,
    snapshot: SnapshotId,
    keys: &[RegionKey],
) -> Result<(), StorageError> {
    if keys.is_empty() {
        return Ok(());
    }
    let mut stmt = conn.prepare(
        "UPDATE region_state SET snapshot_id = ?
          WHERE dim = ? AND kind = ? AND rx = ? AND rz = ?",
    )?;
    for key in keys {
        stmt.execute(params![
            snapshot.0.cast_signed(),
            i64::from(key.dim.raw()),
            i64::from(key.kind.raw()),
            i64::from(key.rx),
            i64::from(key.rz),
        ])?;
    }
    Ok(())
}

/// Drop state rows for region files gone from disk.
pub fn delete_region_states(conn: &Connection, keys: &[RegionKey]) -> Result<(), StorageError> {
    if keys.is_empty() {
        return Ok(());
    }
    let mut stmt =
        conn.prepare("DELETE FROM region_state WHERE dim = ? AND kind = ? AND rx = ? AND rz = ?")?;
    for key in keys {
        stmt.execute(params![
            i64::from(key.dim.raw()),
            i64::from(key.kind.raw()),
            i64::from(key.rx),
            i64::from(key.rz),
        ])?;
    }
    Ok(())
}

/// Carry skipped regions' rows from `prev` into `new_id`; returns rows copied.
///
/// One indexed `INSERT ... SELECT` per region (prefix `snapshot_id, dim,
/// kind` plus chunk ranges), so unchanged regions cost one statement instead
/// of a per-chunk loop. Callers must keep carried regions disjoint from
/// freshly ingested entries; overlap aborts loudly on the primary key.
pub fn carry_region_chunks(
    conn: &Connection,
    new_id: SnapshotId,
    prev: SnapshotId,
    keys: &[RegionKey],
) -> Result<usize, StorageError> {
    if keys.is_empty() {
        return Ok(0);
    }
    let mut stmt = conn.prepare(
        "INSERT INTO chunk_history
         (snapshot_id, dim, kind, cx, cz, blob, diff)
         SELECT ?, dim, kind, cx, cz, blob, diff FROM chunk_history
         WHERE snapshot_id = ? AND dim = ? AND kind = ?
           AND cx BETWEEN ? AND ? AND cz BETWEEN ? AND ?",
    )?;
    let mut carried = 0usize;
    for key in keys {
        // Region `(rx, rz)` owns chunk columns `[rx*32, rx*32+31]`; `i64`
        // math keeps extreme file names from wrapping.
        let lo_x = i64::from(key.rx) * 32;
        let lo_z = i64::from(key.rz) * 32;
        carried += stmt.execute(params![
            new_id.0.cast_signed(),
            prev.0.cast_signed(),
            i64::from(key.dim.raw()),
            i64::from(key.kind.raw()),
            lo_x,
            lo_x + 31,
            lo_z,
            lo_z + 31,
        ])?;
    }
    Ok(carried)
}

/// Wipe all fingerprints; the next backup repopulates them via full ingest.
///
/// Recovery seam for the derived-state invariant: state loss costs one slow
/// backup, never correctness.
pub fn clear_region_states(conn: &Connection) -> Result<(), StorageError> {
    conn.execute("DELETE FROM region_state", [])?;
    Ok(())
}
