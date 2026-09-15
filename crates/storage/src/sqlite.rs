//! SQLite MVCC metadata: snapshots and per-chunk history.
//!
//! Snapshot commits use one transaction; CAS durability is established before
//! metadata commit by the caller. The pool has one connection.

use std::path::Path;

use sekai_core::{
    ApplyOutcome, BlobHash, ChunkCoord, ChunkHistoryEntry, DiffHash, Dimension, MetaStore,
    RegionFingerprint, RegionKey, RegionKind, RegionStateEntry, Snapshot, SnapshotEntry,
    SnapshotId,
};
use sqlx::Row as _;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqliteSynchronous};

use crate::api::{StorageError, Store, io_error};
use crate::cas::FileCas;

/// Managed schema version (`PRAGMA user_version`).
pub const SCHEMA_VERSION: i64 = 4;

const SCHEMA: &str = include_str!("../schema/sqlite.sql");

/// SQLite-backed MVCC metadata.
#[derive(Debug, Clone)]
pub struct SqliteMeta {
    pool: SqlitePool,
}

/// Opened SQLite store: metadata plus file CAS under one root.
pub type SqliteStore = Store<SqliteMeta, FileCas>;

/// Open (creating if needed) the store rooted at `root`
/// (`<root>/meta.sqlite` plus `<root>/blobs/`).
pub async fn open_sqlite(root: &Path) -> Result<SqliteStore, StorageError> {
    std::fs::create_dir_all(root).map_err(|source| io_error(root, source))?;
    let cas = FileCas::open(root)?;
    let meta = SqliteMeta::open(&root.join("meta.sqlite")).await?;
    Ok(Store::new(meta, cas))
}

impl SqliteMeta {
    /// Open (creating and version-gating) the database at `path`.
    ///
    /// The parent directory must already exist; this type never creates it
    /// (directory layout is the store's concern).
    pub async fn open(path: &Path) -> Result<Self, StorageError> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .foreign_keys(true);
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        let version: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&pool)
            .await?;
        if version == 0 {
            sqlx::query(SCHEMA).execute(&pool).await?;
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "PRAGMA user_version = {SCHEMA_VERSION}"
            )))
            .execute(&pool)
            .await?;
        } else if version != SCHEMA_VERSION {
            return Err(StorageError::UnsupportedSchema {
                found: version,
                supported: SCHEMA_VERSION,
            });
        }
        Ok(Self { pool })
    }

    /// Record a whole snapshot atomically; returns its new ID.
    ///
    /// Blobs must already be flushed to CAS before calling.
    pub async fn apply_snapshot(
        &mut self,
        created_at_ms: u64,
        entries: &[SnapshotEntry],
    ) -> Result<SnapshotId, StorageError> {
        Ok(self
            .apply_snapshot_incremental(created_at_ms, entries, None, &[], &[])
            .await?
            .id)
    }
}

/// Fallible `i64` -> `i32` narrowing for SQLite integer columns.
fn i64_to_i32(value: i64, column: &'static str) -> Result<i32, StorageError> {
    i32::try_from(value).map_err(|_| StorageError::InvalidHistoryValue { column, value })
}

/// Fallible `i64` -> `u64` widening for SQLite timestamp/size columns.
fn i64_to_u64(value: i64, column: &'static str) -> Result<u64, StorageError> {
    u64::try_from(value).map_err(|_| StorageError::InvalidHistoryValue { column, value })
}

/// Fallible `i64` row ID -> domain `SnapshotId`.
fn snapshot_id_from_i64(id: i64) -> Result<SnapshotId, StorageError> {
    i64_to_u64(id, "id").map(SnapshotId)
}

/// Read an `i32` column with a loud error on out-of-range values.
fn i32_col(row: &sqlx::sqlite::SqliteRow, column: &'static str) -> Result<i32, StorageError> {
    let value: i64 = row.try_get(column)?;
    i64_to_i32(value, column)
}

/// Read a 32-byte hash column.
fn hash32(bytes: Vec<u8>) -> Result<[u8; 32], StorageError> {
    bytes
        .try_into()
        .map_err(|bytes: Vec<u8>| StorageError::InvalidHashLength { len: bytes.len() })
}

/// Read an optional `BlobHash` from a 32-byte hash column.
fn hash_col(
    row: &sqlx::sqlite::SqliteRow,
    column: &'static str,
) -> Result<Option<BlobHash>, StorageError> {
    let bytes: Option<Vec<u8>> = row.try_get(column)?;
    bytes.map(|bytes| hash32(bytes).map(BlobHash)).transpose()
}

/// Read a millisecond timestamp column.
fn millis_col(row: &sqlx::sqlite::SqliteRow, column: &'static str) -> Result<u64, StorageError> {
    let value: i64 = row.try_get(column)?;
    i64_to_u64(value, column)
}

/// Decode one `chunk_history` row.
fn decode_history(row: &sqlx::sqlite::SqliteRow) -> Result<ChunkHistoryEntry, StorageError> {
    let coord = ChunkCoord::new(
        Dimension::new(i32_col(row, "dim")?),
        RegionKind::new(i32_col(row, "kind")?),
        i32_col(row, "cx")?,
        i32_col(row, "cz")?,
    );
    let blob = hash_col(row, "blob")?;
    let diff_bytes: Option<Vec<u8>> = row.try_get("diff")?;
    let diff = diff_bytes
        .map(|bytes| hash32(bytes).map(DiffHash))
        .transpose()?;
    let snapshot_raw: i64 = row.try_get("snapshot_id")?;
    let snapshot = snapshot_id_from_i64(snapshot_raw)?;
    Ok(ChunkHistoryEntry::new(coord, snapshot, blob, diff))
}

/// Decode one `region_state` row.
fn decode_state(row: &sqlx::sqlite::SqliteRow) -> Result<RegionStateEntry, StorageError> {
    let key = RegionKey::new(
        Dimension::new(i32_col(row, "dim")?),
        RegionKind::new(i32_col(row, "kind")?),
        i32_col(row, "rx")?,
        i32_col(row, "rz")?,
    );
    let mtime_ms: Option<i64> = row.try_get("mtime_ms")?;
    let mtime_ms = mtime_ms.map(|v| i64_to_u64(v, "mtime_ms")).transpose()?;
    let size_raw: i64 = row.try_get("size")?;
    let size = i64_to_u64(size_raw, "size")?;
    let header: Vec<u8> = row.try_get("header_hash")?;
    let header_hash: [u8; 32] = header
        .try_into()
        .map_err(|header: Vec<u8>| StorageError::InvalidHashLength { len: header.len() })?;
    let snapshot_raw: i64 = row.try_get("snapshot_id")?;
    let snapshot_id = snapshot_id_from_i64(snapshot_raw)?;
    Ok(RegionStateEntry {
        key,
        mtime_ms,
        size,
        header_hash,
        snapshot_id,
    })
}

/// `SnapshotId` as an `INTEGER` bind parameter.
fn snap_param(id: SnapshotId) -> Result<i64, StorageError> {
    i64::try_from(id.0).map_err(|_| StorageError::InvalidSnapshotId(id.0))
}

/// Millis as an `INTEGER` parameter. Genuinely fallible: callers compute the
/// clock, so an absurd value is their bug, surfaced loudly.
fn millis_param(ms: u64) -> Result<i64, StorageError> {
    i64::try_from(ms).map_err(|_| StorageError::InvalidTimestamp(ms))
}

impl sekai_core::MetaStore for SqliteMeta {
    type Error = StorageError;

    async fn create_snapshot(&mut self, created_at_ms: u64) -> Result<SnapshotId, StorageError> {
        let id = sqlx::query("INSERT INTO snapshots (created_at_ms) VALUES (?)")
            .bind(millis_param(created_at_ms)?)
            .execute(&self.pool)
            .await?
            .last_insert_rowid();
        snapshot_id_from_i64(id)
    }

    async fn record_chunk(
        &mut self,
        snapshot: SnapshotId,
        coord: &ChunkCoord,
        blob: Option<&BlobHash>,
        diff: Option<&DiffHash>,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO chunk_history (snapshot_id, dim, kind, cx, cz, blob, diff)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(snap_param(snapshot)?)
        .bind(i64::from(coord.dim.raw()))
        .bind(i64::from(coord.kind.raw()))
        .bind(i64::from(coord.x))
        .bind(i64::from(coord.z))
        .bind(blob.map(|h| &h.0[..]))
        .bind(diff.map(|h| &h.0[..]))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn lookup_chunk(
        &self,
        snapshot: SnapshotId,
        coord: &ChunkCoord,
    ) -> Result<Option<ChunkHistoryEntry>, StorageError> {
        // Nearest row at or before `snapshot`: unchanged chunks have no row
        // at newer snapshots (delta storage). The coord index serves the
        // point lookup.
        let row = sqlx::query(
            "SELECT snapshot_id, dim, kind, cx, cz, blob, diff FROM chunk_history
             WHERE dim = ? AND kind = ? AND cx = ? AND cz = ? AND snapshot_id <= ?
             ORDER BY snapshot_id DESC LIMIT 1",
        )
        .bind(i64::from(coord.dim.raw()))
        .bind(i64::from(coord.kind.raw()))
        .bind(i64::from(coord.x))
        .bind(i64::from(coord.z))
        .bind(snap_param(snapshot)?)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| decode_history(&row)).transpose()
    }

    async fn lookup_snapshot(&self, id: SnapshotId) -> Result<Option<Snapshot>, StorageError> {
        let row = sqlx::query("SELECT id, created_at_ms FROM snapshots WHERE id = ?")
            .bind(snap_param(id)?)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| {
            let id: i64 = row.try_get("id")?;
            Ok(Snapshot::new(
                snapshot_id_from_i64(id)?,
                millis_col(&row, "created_at_ms")?,
            ))
        })
        .transpose()
    }

    async fn latest_snapshot(&self) -> Result<Option<Snapshot>, StorageError> {
        let row = sqlx::query("SELECT id, created_at_ms FROM snapshots ORDER BY id DESC LIMIT 1")
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| {
            let id: i64 = row.try_get("id")?;
            Ok(Snapshot::new(
                snapshot_id_from_i64(id)?,
                millis_col(&row, "created_at_ms")?,
            ))
        })
        .transpose()
    }

    async fn visit_snapshot_chunks<F>(
        &self,
        snapshot: SnapshotId,
        mut visit: F,
    ) -> Result<(), StorageError>
    where
        F: FnMut(&ChunkHistoryEntry) -> bool + Send,
    {
        // Effective rows at `snapshot`: the newest row at or before it per
        // coordinate, tombstones included. Window functions keep the query
        // portable across backends. A single fetch beats paged window
        // queries (which recompute the aggregation per page), and every
        // caller materializes the full effective set anyway (rollback
        // groups, GC sets, plan universes).
        let rows = sqlx::query(
            "SELECT dim, kind, cx, cz, blob, diff, snapshot_id FROM (
               SELECT dim, kind, cx, cz, blob, diff, snapshot_id,
                 ROW_NUMBER() OVER (
                   PARTITION BY dim, kind, cx, cz ORDER BY snapshot_id DESC
                 ) AS rn
               FROM chunk_history WHERE snapshot_id <= ?
             ) WHERE rn = 1 ORDER BY dim, kind, cx, cz",
        )
        .bind(snap_param(snapshot)?)
        .fetch_all(&self.pool)
        .await?;
        for row in &rows {
            if !visit(&decode_history(row)?) {
                break;
            }
        }
        Ok(())
    }

    async fn visit_snapshots<F>(&self, mut visit: F) -> Result<(), StorageError>
    where
        F: FnMut(&Snapshot) -> bool + Send,
    {
        let rows = sqlx::query("SELECT id, created_at_ms FROM snapshots ORDER BY id")
            .fetch_all(&self.pool)
            .await?;
        for row in &rows {
            let id: i64 = row.try_get("id")?;
            let snapshot =
                Snapshot::new(snapshot_id_from_i64(id)?, millis_col(row, "created_at_ms")?);
            if !visit(&snapshot) {
                break;
            }
        }
        Ok(())
    }

    async fn load_region_states(&self) -> Result<Vec<RegionStateEntry>, StorageError> {
        let rows = sqlx::query(
            "SELECT dim, kind, rx, rz, mtime_ms, size, header_hash, snapshot_id FROM region_state",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(decode_state).collect()
    }

    async fn apply_snapshot_incremental(
        &mut self,
        created_at_ms: u64,
        entries: &[SnapshotEntry],
        carry_from: Option<(SnapshotId, &[RegionKey])>,
        fingerprints: &[RegionFingerprint],
        removed: &[RegionKey],
    ) -> Result<ApplyOutcome, StorageError> {
        let mut tx = self.pool.begin().await?;
        let id: i64 = sqlx::query("INSERT INTO snapshots (created_at_ms) VALUES (?)")
            .bind(millis_param(created_at_ms)?)
            .execute(&mut *tx)
            .await?
            .last_insert_rowid();
        let snapshot = snapshot_id_from_i64(id)?;
        for entry in entries {
            sqlx::query(
                "INSERT INTO chunk_history (snapshot_id, dim, kind, cx, cz, blob, diff)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(id)
            .bind(i64::from(entry.coord.dim.raw()))
            .bind(i64::from(entry.coord.kind.raw()))
            .bind(i64::from(entry.coord.x))
            .bind(i64::from(entry.coord.z))
            .bind(entry.blob.as_ref().map(|h| &h.0[..]))
            .bind(entry.diff.as_ref().map(|h| &h.0[..]))
            .execute(&mut *tx)
            .await?;
        }
        let mut carried_chunks = 0usize;
        if let Some((prev, keys)) = carry_from {
            // Delta storage: carried regions contribute no rows. Their
            // effective present chunks are counted for the report with the
            // same newest-at-or-before semantics as reads; only derived
            // state advances below.
            if !keys.is_empty() {
                // Region predicates are structural (fixed shape per region);
                // all values stay bound parameters.
                let mut qb = sqlx::QueryBuilder::new(
                    "SELECT COUNT(*) FROM (
                       SELECT ROW_NUMBER() OVER (
                         PARTITION BY dim, kind, cx, cz ORDER BY snapshot_id DESC
                       ) AS rn, blob
                       FROM chunk_history WHERE snapshot_id <= ",
                );
                qb.push_bind(snap_param(prev)?);
                qb.push(" AND (");
                for (i, key) in keys.iter().enumerate() {
                    if i > 0 {
                        qb.push(" OR ");
                    }
                    qb.push("(dim = ");
                    qb.push_bind(i64::from(key.dim.raw()));
                    qb.push(" AND kind = ");
                    qb.push_bind(i64::from(key.kind.raw()));
                    qb.push(" AND cx BETWEEN ");
                    qb.push_bind(i64::from(key.rx) * 32);
                    qb.push(" AND ");
                    qb.push_bind(i64::from(key.rx) * 32 + 31);
                    qb.push(" AND cz BETWEEN ");
                    qb.push_bind(i64::from(key.rz) * 32);
                    qb.push(" AND ");
                    qb.push_bind(i64::from(key.rz) * 32 + 31);
                    qb.push(")");
                }
                qb.push(")) WHERE rn = 1 AND blob IS NOT NULL");
                let carried: i64 = qb.build_query_scalar().fetch_one(&mut *tx).await?;
                carried_chunks = usize::try_from(carried).unwrap_or(usize::MAX);
            }
            for key in keys {
                sqlx::query(
                    "UPDATE region_state SET snapshot_id = ?
                     WHERE dim = ? AND kind = ? AND rx = ? AND rz = ?",
                )
                .bind(id)
                .bind(i64::from(key.dim.raw()))
                .bind(i64::from(key.kind.raw()))
                .bind(i64::from(key.rx))
                .bind(i64::from(key.rz))
                .execute(&mut *tx)
                .await?;
            }
        }
        for fp in fingerprints {
            let mtime_ms: Option<i64> = fp
                .mtime_ms
                .map(|ms| i64::try_from(ms).map_err(|_| StorageError::InvalidTimestamp(ms)))
                .transpose()?;
            let size = i64::try_from(fp.size).unwrap_or(i64::MAX);
            sqlx::query(
                "INSERT INTO region_state (dim, kind, rx, rz, mtime_ms, size, header_hash, snapshot_id)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(dim, kind, rx, rz) DO UPDATE SET
                   mtime_ms = excluded.mtime_ms, size = excluded.size,
                   header_hash = excluded.header_hash, snapshot_id = excluded.snapshot_id",
            )
            .bind(i64::from(fp.key.dim.raw()))
            .bind(i64::from(fp.key.kind.raw()))
            .bind(i64::from(fp.key.rx))
            .bind(i64::from(fp.key.rz))
            .bind(mtime_ms)
            .bind(size)
            .bind(&fp.header_hash[..])
            .bind(id)
            .execute(&mut *tx)
            .await?;
        }
        for key in removed {
            sqlx::query(
                "DELETE FROM region_state WHERE dim = ? AND kind = ? AND rx = ? AND rz = ?",
            )
            .bind(i64::from(key.dim.raw()))
            .bind(i64::from(key.kind.raw()))
            .bind(i64::from(key.rx))
            .bind(i64::from(key.rz))
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(ApplyOutcome {
            id: snapshot,
            carried_chunks,
        })
    }
}

#[cfg(all(test, feature = "backend-sqlite"))]
mod tests {
    use super::*;
    use sekai_core::{
        RegionKind,
        usecase::backup::{Observation, assemble, commit, plan_backup, stage_present},
    };
    use sqlx::ConnectOptions as _;
    use sqlx::Connection as _;
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn tempdir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sekai-{}-{}-{}",
            name,
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    async fn memory_meta() -> SqliteMeta {
        SqliteMeta::open(&std::path::PathBuf::from(":memory:"))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn fresh_store_carries_schema_version() {
        let meta = memory_meta().await;
        let version: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&meta.pool)
            .await
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[tokio::test]
    async fn version_gate_rejects_foreign_schema() {
        let dir = tempdir("version");
        let path = dir.join("meta.sqlite");
        let mut seed = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .connect()
            .await
            .unwrap();
        sqlx::query(SCHEMA).execute(&mut seed).await.unwrap();
        sqlx::query("PRAGMA user_version=999")
            .execute(&mut seed)
            .await
            .unwrap();
        seed.close().await.unwrap();
        assert!(matches!(
            SqliteMeta::open(&path).await,
            Err(StorageError::UnsupportedSchema {
                found: 999,
                supported: SCHEMA_VERSION,
            })
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn wiped_derived_state_degrades_to_full_ingest() {
        let mut meta = memory_meta().await;
        let key = RegionKey::new(Dimension::OVERWORLD, RegionKind::REGION, 0, 0);
        let fp = RegionFingerprint {
            key,
            mtime_ms: Some(1),
            size: 8192,
            header_hash: [7; 32],
        };
        let coord = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::REGION, 0, 0);
        meta.apply_snapshot_incremental(
            1_000,
            &[stage_present(coord, BlobHash([1; 32]))],
            None,
            &[fp],
            &[],
        )
        .await
        .unwrap();
        sqlx::query("DELETE FROM region_state")
            .execute(&meta.pool)
            .await
            .unwrap();
        let (previous, plan) = plan_backup(
            &meta,
            &[Observation {
                key,
                fingerprint: fp,
            }],
        )
        .await
        .unwrap();
        assert!(plan.carries.is_empty());
        assert_eq!(plan.ingest, vec![key]);
        let staged = assemble(
            plan,
            &previous,
            vec![stage_present(coord, BlobHash([1; 32]))],
            BTreeSet::from([coord]),
            0,
            vec![fp],
        );
        let report = commit(&mut meta, &previous, &staged, 2_000).await.unwrap();
        assert_eq!(report.carried_chunks, 0);
        assert_eq!(report.skipped_regions, 0);
    }

    #[tokio::test]
    async fn carry_writes_no_chunk_rows() {
        use sekai_core::MetaStore as _;
        let mut meta = memory_meta().await;
        let key0 = RegionKey::new(Dimension::OVERWORLD, RegionKind::REGION, 0, 0);
        let key1 = RegionKey::new(Dimension::OVERWORLD, RegionKind::REGION, 1, 0);
        let fp = |key| RegionFingerprint {
            key,
            mtime_ms: Some(1),
            size: 8192,
            header_hash: [7; 32],
        };
        let c0 = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::REGION, 0, 0);
        let c1 = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::REGION, 32, 0);
        meta.apply_snapshot_incremental(
            1_000,
            &[
                stage_present(c0, BlobHash([1; 32])),
                stage_present(c1, BlobHash([2; 32])),
            ],
            None,
            &[fp(key0), fp(key1)],
            &[],
        )
        .await
        .unwrap();

        // Second backup: region 0 changes, region 1 carries.
        let mut changed = fp(key0);
        changed.size += 1;
        let (previous, plan) = plan_backup(
            &meta,
            &[
                Observation {
                    key: key0,
                    fingerprint: changed,
                },
                Observation {
                    key: key1,
                    fingerprint: fp(key1),
                },
            ],
        )
        .await
        .unwrap();
        let staged = assemble(
            plan,
            &previous,
            vec![stage_present(c0, BlobHash([9; 32]))],
            BTreeSet::from([c0]),
            1,
            vec![changed],
        );
        let report = commit(&mut meta, &previous, &staged, 2_000).await.unwrap();
        assert_eq!(report.carried_chunks, 1);

        // Physical rows at the new snapshot: the fresh entry only. The
        // carried chunk stays readable through fallback.
        let physical: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM chunk_history WHERE snapshot_id = ?")
                .bind(snap_param(report.snapshot).unwrap())
                .fetch_one(&meta.pool)
                .await
                .unwrap();
        assert_eq!(physical, 1);
        let kept = meta
            .lookup_chunk(report.snapshot, &c1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(kept.blob, Some(BlobHash([2; 32])));
        assert_eq!(kept.snapshot, SnapshotId(1));
    }
}
