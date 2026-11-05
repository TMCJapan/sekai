//! Typed row documents exchanged with `SurrealDB`, plus conversions to the
//! domain types in `sekai-core`.
//!
//! Every table is read and written through a `SurrealValue` struct so the
//! derive macro owns field decoding; only the domain validation (hash length,
//! region kind, ref/scope name grammar) stays explicit.

use std::fmt;

use sekai_core::{
    ChunkCoord, ChunkHash, ChunkHistoryEntry, CommitId, CommitRef, Dimension, NamedScope,
    NamedScopeError, Rect, RefName, RegionKind, ScopeName,
};
use sekai_storage::StorageError;
// `Value` is named by the expansion of `#[surreal(flatten)]` below, not by
// this module directly.
use surrealdb::types::{Bytes, RecordId, SurrealValue, Value};

/// Record ID of a blob, keyed by its lowercase hex hash.
pub(crate) fn blob_record(hash: &ChunkHash) -> RecordId {
    RecordId::new("blob", hash.hex_string())
}

/// Record ID of a commit, keyed by its lowercase hex ID.
pub(crate) fn commit_record(id: CommitId) -> RecordId {
    RecordId::new("commit", id.hex_string())
}

/// Record ID of a ref, keyed by its validated name.
pub(crate) fn ref_record(name: &RefName) -> RecordId {
    ref_record_name(name.as_str())
}

/// Record ID of a ref from a name already known to be valid.
pub(crate) fn ref_record_name(name: &str) -> RecordId {
    RecordId::new("ref", name)
}

/// Record ID of a scope, keyed by its validated name.
pub(crate) fn scope_record(name: &ScopeName) -> RecordId {
    RecordId::new("scope", name.as_str())
}

/// Map a backend failure into the shared storage error.
pub(crate) fn db_err(source: surrealdb::Error) -> StorageError {
    StorageError::backend("surrealdb", source)
}

/// Failure while converting a typed row into its domain value.
#[derive(Debug)]
struct DataError(String);

impl fmt::Display for DataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl core::error::Error for DataError {}

/// A row that cannot be interpreted as the value type it must hold.
pub(crate) fn corrupt(message: impl Into<String>) -> StorageError {
    StorageError::backend("surrealdb", DataError(message.into()))
}

/// Encode a chunk hash for a `SurrealDB` `bytes` field.
pub(crate) fn hash_bytes(hash: &ChunkHash) -> Bytes {
    Bytes::from(hash.as_bytes().to_vec())
}

/// Encode a commit ID for a `SurrealDB` `bytes` field.
pub(crate) fn commit_bytes(id: CommitId) -> Bytes {
    Bytes::from(id.0.to_vec())
}

fn decode_hash(bytes: Bytes) -> Result<[u8; 32], StorageError> {
    bytes
        .into_inner()
        .to_vec()
        .try_into()
        .map_err(|bytes: Vec<u8>| StorageError::InvalidHashLength { len: bytes.len() })
}

fn decode_commit(bytes: Bytes) -> Result<CommitId, StorageError> {
    decode_hash(bytes).map(CommitId)
}

fn decode_optional_hash(bytes: Option<Bytes>) -> Result<Option<ChunkHash>, StorageError> {
    bytes
        .map(|bytes| decode_hash(bytes).map(ChunkHash::new))
        .transpose()
}

/// Reject a timestamp that does not fit the signed integer `SurrealDB` stores.
fn checked_millis(ms: u64) -> Result<u64, StorageError> {
    i64::try_from(ms)
        .map(|_| ms)
        .map_err(|_| StorageError::InvalidTimestamp(ms))
}

pub(crate) fn parent_bytes(parents: &[CommitId]) -> Vec<Bytes> {
    parents.iter().copied().map(commit_bytes).collect()
}

fn parent_ids(bytes: Vec<Bytes>) -> Result<Vec<CommitId>, StorageError> {
    bytes.into_iter().map(decode_commit).collect()
}

/// The singleton `meta:store` document.
#[derive(Debug, SurrealValue)]
pub(crate) struct MetaRow {
    pub(crate) schema_version: i64,
}

/// A blob document; the record ID is the hex hash.
#[derive(Debug, SurrealValue)]
pub(crate) struct BlobRow {
    pub(crate) hash: Bytes,
    pub(crate) payload: Bytes,
}

impl BlobRow {
    pub(crate) fn new(hash: &ChunkHash, payload: &[u8]) -> Self {
        Self {
            hash: hash_bytes(hash),
            payload: Bytes::from(payload.to_vec()),
        }
    }

    pub(crate) fn into_parts(self) -> Result<(ChunkHash, Vec<u8>), StorageError> {
        Ok((
            ChunkHash::new(decode_hash(self.hash)?),
            self.payload.into_inner().to_vec(),
        ))
    }
}

/// A blob hash projection used when visiting the CAS without payloads.
#[derive(Debug, SurrealValue)]
pub(crate) struct BlobHashRow {
    pub(crate) hash: Bytes,
}

impl BlobHashRow {
    pub(crate) fn into_hash(self) -> Result<ChunkHash, StorageError> {
        decode_hash(self.hash).map(ChunkHash::new)
    }
}

/// A row that only needs its record ID (existence probes).
#[derive(Debug, SurrealValue)]
pub(crate) struct IdRow {
    pub(crate) id: RecordId,
}

/// The stored commit fields shared by reads and writes.
#[derive(Debug, SurrealValue)]
pub(crate) struct CommitRow {
    pub(crate) hash: Bytes,
    pub(crate) seq: i64,
    pub(crate) created_at_ms: u64,
    pub(crate) parents: Vec<Bytes>,
}

impl CommitRow {
    pub(crate) fn new(
        id: CommitId,
        seq: i64,
        created_at_ms: u64,
        parents: &[CommitId],
    ) -> Result<Self, StorageError> {
        Ok(Self {
            hash: commit_bytes(id),
            seq,
            created_at_ms: checked_millis(created_at_ms)?,
            parents: parent_bytes(parents),
        })
    }

    pub(crate) fn into_stored(self) -> Result<StoredCommit, StorageError> {
        Ok(StoredCommit {
            id: decode_commit(self.hash)?,
            seq: self.seq,
            created_at_ms: self.created_at_ms,
            parents: parent_ids(self.parents)?,
        })
    }
}

/// A commit row with its local sequence, after domain validation.
pub(crate) struct StoredCommit {
    pub(crate) id: CommitId,
    pub(crate) seq: i64,
    pub(crate) created_at_ms: u64,
    pub(crate) parents: Vec<CommitId>,
}

impl StoredCommit {
    pub(crate) fn into_commit(self) -> sekai_core::Commit {
        sekai_core::Commit::new(self.id, self.created_at_ms, self.parents)
    }
}

/// A commit patch that changes only the parent list.
#[derive(Debug, SurrealValue)]
pub(crate) struct CommitParentsPatch {
    pub(crate) parents: Vec<Bytes>,
}

impl CommitParentsPatch {
    pub(crate) fn new(parents: &[CommitId]) -> Self {
        Self {
            parents: parent_bytes(parents),
        }
    }
}

/// The fields shared by a stored history row and a fresh insert.
#[derive(Debug, SurrealValue)]
pub(crate) struct HistoryRow {
    pub(crate) commit: Bytes,
    pub(crate) seq: i64,
    pub(crate) dim: i32,
    pub(crate) kind: i32,
    pub(crate) cx: i32,
    pub(crate) cz: i32,
    pub(crate) blob: Option<Bytes>,
}

impl HistoryRow {
    pub(crate) fn new(
        commit: CommitId,
        seq: i64,
        coord: ChunkCoord,
        blob: Option<&ChunkHash>,
    ) -> Self {
        Self {
            commit: commit_bytes(commit),
            seq,
            dim: coord.dim.raw(),
            kind: i32::from(coord.kind.raw()),
            cx: coord.x,
            cz: coord.z,
            blob: blob.map(hash_bytes),
        }
    }
}

/// A stored history row, including the record ID needed by prune rewrites.
#[derive(Debug, SurrealValue)]
pub(crate) struct StoredHistoryRow {
    pub(crate) id: RecordId,
    #[surreal(flatten)]
    pub(crate) row: HistoryRow,
}

impl StoredHistoryRow {
    pub(crate) fn into_stored(self) -> Result<StoredHistory, StorageError> {
        let kind = RegionKind::new(self.row.kind).map_err(|_| StorageError::UnknownRegionKind {
            kind: self.row.kind,
        })?;
        let entry = ChunkHistoryEntry::new(
            ChunkCoord::new(Dimension::new(self.row.dim), kind, self.row.cx, self.row.cz),
            decode_commit(self.row.commit)?,
            decode_optional_hash(self.row.blob)?,
        );
        Ok(StoredHistory {
            record: self.id,
            entry,
        })
    }
}

/// A chunk-history row with its record ID, after domain validation.
pub(crate) struct StoredHistory {
    pub(crate) record: RecordId,
    pub(crate) entry: ChunkHistoryEntry,
}

/// A history patch that re-stamps a folded row onto its successor.
#[derive(Debug, SurrealValue)]
pub(crate) struct HistoryMove {
    pub(crate) commit: Bytes,
    pub(crate) seq: i64,
}

impl HistoryMove {
    pub(crate) fn new(commit: CommitId, seq: i64) -> Self {
        Self {
            commit: commit_bytes(commit),
            seq,
        }
    }
}

/// The coordinate columns of a history row.
#[derive(Debug, SurrealValue)]
pub(crate) struct CoordRow {
    pub(crate) dim: i32,
    pub(crate) kind: i32,
    pub(crate) cx: i32,
    pub(crate) cz: i32,
}

impl CoordRow {
    pub(crate) const fn into_key(self) -> (i32, i32, i32, i32) {
        (self.dim, self.kind, self.cx, self.cz)
    }
}

/// A ref document; the record ID is the validated ref name.
#[derive(Debug, SurrealValue)]
pub(crate) struct RefRow {
    pub(crate) name: String,
    pub(crate) commit: Option<Bytes>,
    pub(crate) active: bool,
    pub(crate) created_at_ms: u64,
}

impl RefRow {
    pub(crate) fn new(
        name: &RefName,
        commit: Option<CommitId>,
        active: bool,
        created_at_ms: u64,
    ) -> Result<Self, StorageError> {
        Ok(Self {
            name: name.as_str().to_owned(),
            commit: commit.map(commit_bytes),
            active,
            created_at_ms: checked_millis(created_at_ms)?,
        })
    }

    pub(crate) fn new_default(created_at_ms: u64) -> Result<Self, StorageError> {
        Ok(Self {
            name: sekai_core::DEFAULT_REF_NAME.to_owned(),
            commit: None,
            active: true,
            created_at_ms: checked_millis(created_at_ms)?,
        })
    }

    pub(crate) fn into_domain(self) -> Result<CommitRef, StorageError> {
        let name = RefName::parse(&self.name)
            .map_err(|_| StorageError::InvalidRefName { name: self.name })?;
        Ok(CommitRef::new(
            name,
            self.commit.map(decode_commit).transpose()?,
            self.active,
            self.created_at_ms,
        ))
    }
}

/// A ref patch that changes only the target commit.
#[derive(Debug, SurrealValue)]
pub(crate) struct RefCommitPatch {
    pub(crate) commit: Option<Bytes>,
}

impl RefCommitPatch {
    pub(crate) fn new(commit: Option<CommitId>) -> Self {
        Self {
            commit: commit.map(commit_bytes),
        }
    }
}

/// A ref patch that changes only the active flag.
#[derive(Debug, SurrealValue)]
pub(crate) struct RefActivePatch {
    pub(crate) active: bool,
}

/// One rectangle of a named scope.
#[derive(Debug, SurrealValue)]
pub(crate) struct AreaRow {
    pub(crate) dim: i32,
    pub(crate) x0: i32,
    pub(crate) z0: i32,
    pub(crate) x1: i32,
    pub(crate) z1: i32,
}

/// A named scope document; the record ID is the validated scope name.
#[derive(Debug, SurrealValue)]
pub(crate) struct ScopeRow {
    pub(crate) name: String,
    pub(crate) created_at_ms: u64,
    pub(crate) areas: Vec<AreaRow>,
}

impl ScopeRow {
    pub(crate) fn new(scope: &NamedScope, created_at_ms: u64) -> Result<Self, StorageError> {
        Ok(Self {
            name: scope.name.as_str().to_owned(),
            created_at_ms: checked_millis(created_at_ms)?,
            areas: scope
                .areas()
                .iter()
                .map(|(dim, rect)| AreaRow {
                    dim: dim.raw(),
                    x0: rect.x0,
                    z0: rect.z0,
                    x1: rect.x1,
                    z1: rect.z1,
                })
                .collect(),
        })
    }

    pub(crate) fn into_domain(self) -> Result<NamedScope, StorageError> {
        let name = ScopeName::parse(&self.name).map_err(|_| StorageError::InvalidScopeName {
            name: self.name.clone(),
        })?;
        let areas = self
            .areas
            .into_iter()
            .map(|area| {
                (
                    Dimension::new(area.dim),
                    Rect::new(area.x0, area.z0, area.x1, area.z1),
                )
            })
            .collect();
        NamedScope::new(name, areas, self.created_at_ms)
            .map_err(|NamedScopeError::NoAreas| StorageError::EmptyScope { name: self.name })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_round_trip_preserves_bytes() {
        let hash = ChunkHash::new([0xAB; 32]);
        let row = BlobRow::new(&hash, b"payload");
        let (decoded, payload) = row.into_parts().unwrap();
        assert_eq!(decoded, hash);
        assert_eq!(payload, b"payload");
    }

    #[test]
    fn wrong_hash_length_is_rejected() {
        let row = BlobRow {
            hash: Bytes::from(vec![1, 2, 3]),
            payload: Bytes::from(Vec::new()),
        };
        assert!(matches!(
            row.into_parts(),
            Err(StorageError::InvalidHashLength { len: 3 })
        ));
    }
}
