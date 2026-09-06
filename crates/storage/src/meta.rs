//! SQLite MVCC metadata: snapshots and per-chunk history.
//!
//! Rationale: history rows are plain `(snapshot, coord) -> (blob?, diff?)`
//! facts; interval semantics (`[S_i, S_{i+1})`) emerge from snapshot order,
//! so no range columns are stored. Hashes are 32-byte BLOBs (`NULL` blob =
//! tombstone, `NULL` diff = not computed). `chunk_state`-style derived
//! indices are intentionally absent - they rebuild from this table.
//!
//! Crash posture: `PRAGMA journal_mode=WAL, synchronous=FULL` plus one
//! SQLite transaction per snapshot batch (`apply_snapshot`), so a torn
//! backup never leaves a half-recorded snapshot behind. The CAS-before-DB
//! half of the ordering is `engine`'s responsibility.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension as _, params};
use sekai_core::{BlobHash, ChunkCoord, DiffHash, Snapshot, SnapshotId};

use crate::error::StorageError;

/// Schema version managed by this binary (`PRAGMA user_version`).
///
/// Version 2 adds the derived `region_state` fingerprint cache for
/// incremental snapshots. Version 1 stores are rejected (recreate them):
/// the project is pre-release, so history migration is out of scope and a
/// loud version gate replaces it.
pub const SCHEMA_VERSION: i64 = 2;

const SCHEMA: &str = "
CREATE TABLE snapshots(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at_ms INTEGER NOT NULL
);
CREATE TABLE chunk_history(
    snapshot_id INTEGER NOT NULL REFERENCES snapshots(id),
    dim INTEGER NOT NULL,
    kind INTEGER NOT NULL,
    cx INTEGER NOT NULL,
    cz INTEGER NOT NULL,
    blob BLOB NULL,
    diff BLOB NULL,
    PRIMARY KEY(snapshot_id, dim, kind, cx, cz)
);
CREATE INDEX idx_history_coord
    ON chunk_history(dim, kind, cx, cz, snapshot_id);
-- Derived per-region fingerprints (see `region.rs`): which snapshot last
-- ingested each file and what the file looked like. Rebuilt lazily by the
-- next backup when wiped, so it never needs data migration.
CREATE TABLE region_state(
    dim INTEGER NOT NULL,
    kind INTEGER NOT NULL,
    rx INTEGER NOT NULL,
    rz INTEGER NOT NULL,
    mtime_ms INTEGER NULL,
    size INTEGER NOT NULL,
    header_hash BLOB NOT NULL,
    snapshot_id INTEGER NOT NULL REFERENCES snapshots(id),
    PRIMARY KEY(dim, kind, rx, rz)
);
";

/// One chunk fact for [`SqliteMeta::apply_snapshot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotEntry {
    /// Which chunk this fact describes.
    pub coord: ChunkCoord,
    /// Exact CAS key, or `None` for a tombstone.
    pub blob: Option<BlobHash>,
    /// Cached volatile hash, or `None` when not computed.
    pub diff: Option<DiffHash>,
}

impl SnapshotEntry {
    /// Construct a chunk fact.
    #[must_use]
    pub const fn new(coord: ChunkCoord, blob: Option<BlobHash>, diff: Option<DiffHash>) -> Self {
        Self { coord, blob, diff }
    }
}

/// SQLite-backed MVCC metadata.
#[derive(Debug)]
pub struct SqliteMeta {
    /// Single-writer connection (the CLI never shares it across threads).
    conn: Connection,
}

impl SqliteMeta {
    /// Open (creating and migrating) the database at `path`.
    ///
    /// The parent directory must already exist; this type never creates it
    /// (directory layout is `engine`'s concern). `":memory:"` works for
    /// tests.
    pub fn open(path: &Path) -> Result<Self, StorageError> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=FULL;
             PRAGMA foreign_keys=ON;",
        )?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version == 0 {
            conn.execute_batch(SCHEMA)?;
            conn.execute_batch("PRAGMA user_version=2")?;
        } else if version != SCHEMA_VERSION {
            return Err(StorageError::UnsupportedSchema {
                found: version,
                supported: SCHEMA_VERSION,
            });
        }
        Ok(Self { conn })
    }

    /// Record a whole snapshot atomically; returns its new ID.
    ///
    /// This is the write path `engine` uses: all rows land in one
    /// transaction, so a crash records either the full snapshot or none of
    /// it. Blobs must already be flushed to CAS before calling.
    pub fn apply_snapshot(
        &mut self,
        created_at_ms: u64,
        entries: &[SnapshotEntry],
    ) -> Result<SnapshotId, StorageError> {
        Ok(self
            .apply_snapshot_incremental(created_at_ms, entries, None, &[], &[])?
            .id)
    }

    /// Record a snapshot, carrying unchanged regions from `carry_from`.
    ///
    /// `entries` holds freshly ingested chunks and tombstones; `carry_from`
    /// names the previous snapshot plus the regions whose rows copy over via
    /// `INSERT ... SELECT`. `fingerprints` refreshes the derived
    /// `region_state` for ingested files and `removed` drops state for files
    /// gone from disk, all inside the same transaction so state and history
    /// stay consistent. Carried regions must be disjoint from `entries`;
    /// overlap aborts on the primary key instead of merging silently.
    pub fn apply_snapshot_incremental(
        &mut self,
        created_at_ms: u64,
        entries: &[SnapshotEntry],
        carry_from: Option<(SnapshotId, &[crate::RegionKey])>,
        fingerprints: &[crate::RegionFingerprint],
        removed: &[crate::RegionKey],
    ) -> Result<ApplyOutcome, StorageError> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO snapshots(created_at_ms) VALUES (?)",
            params![i64::try_from(created_at_ms).unwrap_or(i64::MAX)],
        )?;
        // AUTOINCREMENT rowids are always positive.
        let id = SnapshotId(tx.last_insert_rowid().cast_unsigned());
        // One prepared statement for all rows: re-parsing the same SQL per
        // chunk dominated `db_apply` on large worlds.
        let mut stmt = tx.prepare(INSERT_HISTORY_SQL)?;
        for entry in entries {
            // Hashes travel as 32-byte BLOBs (`NULL` blob = tombstone).
            let blob: Option<&[u8]> = entry.blob.as_ref().map(|b| b.0.as_slice());
            let diff: Option<&[u8]> = entry.diff.as_ref().map(|d| d.0.as_slice());
            stmt.execute(params![
                id.0.cast_signed(),
                i64::from(entry.coord.dim.raw()),
                i64::from(entry.coord.kind.raw()),
                i64::from(entry.coord.x),
                i64::from(entry.coord.z),
                blob,
                diff,
            ])?;
        }
        drop(stmt);
        let carried_chunks = if let Some((prev, keys)) = carry_from {
            crate::region::carry_region_chunks(&tx, id, prev, keys)?
        } else {
            0usize
        };
        crate::region::insert_region_states(&tx, id, fingerprints)?;
        crate::region::delete_region_states(&tx, removed)?;
        tx.commit()?;
        Ok(ApplyOutcome { id, carried_chunks })
    }

    /// Load every stored region fingerprint (for skip decisions in `engine`).
    pub fn load_region_states(&self) -> Result<Vec<crate::RegionStateEntry>, StorageError> {
        crate::region::load_region_states(&self.conn)
    }

    /// Wipe derived region fingerprints; the next backup repopulates them.
    ///
    /// Recovery seam for the derived-state invariant (see `region.rs`).
    pub fn reset_region_state(&mut self) -> Result<(), StorageError> {
        crate::region::clear_region_states(&self.conn)
    }
}

/// Outcome of [`SqliteMeta::apply_snapshot_incremental`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplyOutcome {
    /// Newly recorded snapshot ID.
    pub id: SnapshotId,
    /// History rows carried over from the previous snapshot.
    pub carried_chunks: usize,
}

/// Shared INSERT text for history rows (batched and single-row paths).
const INSERT_HISTORY_SQL: &str = "INSERT INTO chunk_history
         (snapshot_id, dim, kind, cx, cz, blob, diff)
         VALUES (?, ?, ?, ?, ?, ?, ?)";

/// Insert one history row on `conn` (shared by the batched and single-row
/// write paths so both encode coordinates and hashes identically).
///
/// Plain INSERT (not REPLACE): recording the same coordinate twice in one
/// snapshot is a caller bug and must surface, never merge.
fn insert_history_row(
    conn: &Connection,
    snapshot: SnapshotId,
    coord: &ChunkCoord,
    blob: Option<&BlobHash>,
    diff: Option<&DiffHash>,
) -> Result<(), StorageError> {
    // Hashes travel as 32-byte BLOBs (`NULL` blob = tombstone).
    let blob: Option<&[u8]> = blob.map(|b| b.0.as_slice());
    let diff: Option<&[u8]> = diff.map(|d| d.0.as_slice());
    conn.execute(
        INSERT_HISTORY_SQL,
        params![
            snapshot.0.cast_signed(),
            i64::from(coord.dim.raw()),
            i64::from(coord.kind.raw()),
            i64::from(coord.x),
            i64::from(coord.z),
            blob,
            diff,
        ],
    )?;
    Ok(())
}

/// Raw history columns as read from SQLite
/// (`dim, kind, cx, cz, blob, diff`).
type HistoryColumns = (i64, i64, i64, i64, Option<Vec<u8>>, Option<Vec<u8>>);

/// 32-byte hash bytes from a BLOB column.
fn hash32(bytes: Vec<u8>) -> Result<[u8; 32], StorageError> {
    <[u8; 32]>::try_from(bytes).map_err(|left| StorageError::InvalidHashLength { len: left.len() })
}

/// History row from columns `(dim, kind, cx, cz, blob, diff)`.
///
/// Integers travel as `i64` (SQLite's native width) and are validated back
/// into their domain types: out-of-range values surface as corruption
/// instead of silently truncating into wrong coordinates.
fn history_row(
    snapshot: SnapshotId,
    dim: i64,
    kind: i64,
    cx: i64,
    cz: i64,
    blob: Option<Vec<u8>>,
    diff: Option<Vec<u8>>,
) -> Result<sekai_core::ChunkHistoryEntry, StorageError> {
    use sekai_core::{ChunkHistoryEntry, Dimension, RegionKind};
    let invalid =
        |column: &'static str, value: i64| StorageError::InvalidHistoryValue { column, value };
    let dim = i32::try_from(dim).map_err(|_| invalid("dim", dim))?;
    let kind = u8::try_from(kind).map_err(|_| invalid("kind", kind))?;
    let cx = i32::try_from(cx).map_err(|_| invalid("cx", cx))?;
    let cz = i32::try_from(cz).map_err(|_| invalid("cz", cz))?;
    Ok(ChunkHistoryEntry::new(
        ChunkCoord::new(Dimension(dim), RegionKind(kind), cx, cz),
        snapshot,
        blob.map(hash32).transpose()?.map(BlobHash),
        diff.map(hash32).transpose()?.map(DiffHash),
    ))
}

impl sekai_core::MetaStore for SqliteMeta {
    type Error = StorageError;

    fn create_snapshot(&mut self, created_at_ms: u64) -> Result<SnapshotId, Self::Error> {
        self.conn.execute(
            "INSERT INTO snapshots(created_at_ms) VALUES (?)",
            params![i64::try_from(created_at_ms).unwrap_or(i64::MAX)],
        )?;
        Ok(SnapshotId(self.conn.last_insert_rowid().cast_unsigned()))
    }

    fn record_chunk(
        &mut self,
        snapshot: SnapshotId,
        coord: &ChunkCoord,
        blob: Option<&BlobHash>,
        diff: Option<&DiffHash>,
    ) -> Result<(), Self::Error> {
        insert_history_row(&self.conn, snapshot, coord, blob, diff)
    }

    fn lookup_chunk(
        &self,
        snapshot: SnapshotId,
        coord: &ChunkCoord,
    ) -> Result<Option<sekai_core::ChunkHistoryEntry>, Self::Error> {
        let found: Option<HistoryColumns> = self
            .conn
            .query_row(
                "SELECT dim, kind, cx, cz, blob, diff FROM chunk_history
                 WHERE snapshot_id = ? AND dim = ? AND kind = ? AND cx = ? AND cz = ?",
                params![
                    snapshot.0.cast_signed(),
                    i64::from(coord.dim.raw()),
                    i64::from(coord.kind.raw()),
                    i64::from(coord.x),
                    i64::from(coord.z),
                ],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()?;
        found
            .map(|(dim, kind, cx, cz, blob, diff)| {
                history_row(snapshot, dim, kind, cx, cz, blob, diff)
            })
            .transpose()
    }

    fn lookup_snapshot(&self, id: SnapshotId) -> Result<Option<Snapshot>, Self::Error> {
        let created_at_ms: Option<i64> = self
            .conn
            .query_row(
                "SELECT created_at_ms FROM snapshots WHERE id = ?",
                params![id.0.cast_signed()],
                |row| row.get(0),
            )
            .optional()?;
        // IDs originate from AUTOINCREMENT rowids, always positive.
        Ok(created_at_ms.map(|ms| Snapshot::new(id, ms.cast_unsigned())))
    }

    fn latest_snapshot(&self) -> Result<Option<Snapshot>, Self::Error> {
        let row: Option<(i64, i64)> = self
            .conn
            .query_row(
                "SELECT id, created_at_ms FROM snapshots ORDER BY id DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(row.map(|(id, created_at_ms)| {
            Snapshot::new(
                SnapshotId(id.cast_unsigned()),
                created_at_ms.cast_unsigned(),
            )
        }))
    }

    fn visit_snapshot_chunks<F>(
        &self,
        snapshot: SnapshotId,
        mut visit: F,
    ) -> Result<(), Self::Error>
    where
        F: FnMut(&sekai_core::ChunkHistoryEntry) -> bool,
    {
        let mut stmt = self.conn.prepare(
            "SELECT dim, kind, cx, cz, blob, diff FROM chunk_history
             WHERE snapshot_id = ? ORDER BY dim, kind, cx, cz",
        )?;
        let mut rows = stmt.query(params![snapshot.0.cast_signed()])?;
        while let Some(row) = rows.next()? {
            let entry = history_row(
                snapshot,
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            )?;
            if !visit(&entry) {
                break;
            }
        }
        Ok(())
    }

    fn visit_snapshots<F>(&self, mut visit: F) -> Result<(), Self::Error>
    where
        F: FnMut(&Snapshot) -> bool,
    {
        let mut stmt = self
            .conn
            .prepare("SELECT id, created_at_ms FROM snapshots ORDER BY id")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let created_at_ms: i64 = row.get(1)?;
            // Both columns originate from non-negative writes.
            if !visit(&Snapshot::new(
                SnapshotId(id.cast_unsigned()),
                created_at_ms.cast_unsigned(),
            )) {
                break;
            }
        }
        Ok(())
    }
}
