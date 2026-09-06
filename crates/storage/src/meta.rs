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
pub(crate) const SCHEMA_VERSION: i64 = 1;

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
            conn.execute_batch("PRAGMA user_version=1")?;
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
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO snapshots(created_at_ms) VALUES (?)",
            params![i64::try_from(created_at_ms).unwrap_or(i64::MAX)],
        )?;
        // AUTOINCREMENT rowids are always positive.
        let id = SnapshotId(tx.last_insert_rowid() as u64);
        {
            let mut stmt = tx.prepare(
                "INSERT INTO chunk_history
                 (snapshot_id, dim, kind, cx, cz, blob, diff)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )?;
            for entry in entries {
                let blob: Option<&[u8]> = entry.blob.as_ref().map(|b| b.0.as_slice());
                let diff: Option<&[u8]> = entry.diff.as_ref().map(|d| d.0.as_slice());
                stmt.execute(params![
                    id.0 as i64,
                    entry.coord.dim.raw() as i64,
                    entry.coord.kind.raw() as i64,
                    entry.coord.x as i64,
                    entry.coord.z as i64,
                    blob,
                    diff,
                ])?;
            }
        }
        tx.commit()?;
        Ok(id)
    }
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
/// Integers travel as `i64` (SQLite's native width) and narrow back: every
/// value was written from a narrower type, so the casts are lossless.
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
    Ok(ChunkHistoryEntry::new(
        ChunkCoord::new(
            Dimension(dim as i32),
            RegionKind(kind as u8),
            cx as i32,
            cz as i32,
        ),
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
        Ok(SnapshotId(self.conn.last_insert_rowid() as u64))
    }

    fn record_chunk(
        &mut self,
        snapshot: SnapshotId,
        coord: &ChunkCoord,
        blob: Option<&BlobHash>,
        diff: Option<&DiffHash>,
    ) -> Result<(), Self::Error> {
        // Plain INSERT (not REPLACE): recording the same coordinate twice
        // in one snapshot is a caller bug and must surface, never merge.
        let blob: Option<&[u8]> = blob.map(|b| b.0.as_slice());
        let diff: Option<&[u8]> = diff.map(|d| d.0.as_slice());
        self.conn.execute(
            "INSERT INTO chunk_history
             (snapshot_id, dim, kind, cx, cz, blob, diff)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            params![
                snapshot.0 as i64,
                coord.dim.raw() as i64,
                coord.kind.raw() as i64,
                coord.x as i64,
                coord.z as i64,
                blob,
                diff,
            ],
        )?;
        Ok(())
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
                    snapshot.0 as i64,
                    coord.dim.raw() as i64,
                    coord.kind.raw() as i64,
                    coord.x as i64,
                    coord.z as i64,
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
        let mut rows = stmt.query(params![snapshot.0 as i64])?;
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
            if !visit(&Snapshot::new(SnapshotId(id as u64), created_at_ms as u64)) {
                break;
            }
        }
        Ok(())
    }
}
