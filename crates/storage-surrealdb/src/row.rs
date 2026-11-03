//! Conversion between `SurrealDB` values and Sekai's shared value types.

use std::fmt;

use sekai_core::{
    ChunkCoord, ChunkHash, ChunkHistoryEntry, CommitId, CommitRef, Dimension, NamedScope,
    NamedScopeError, Rect, RefName, RegionKind, ScopeName,
};
use sekai_storage::StorageError;
use surrealdb::types::{Bytes, Number, Object, RecordId, Value};

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
    RecordId::new("ref", name.as_str())
}

/// Record ID of a scope, keyed by its validated name.
pub(crate) fn scope_record(name: &ScopeName) -> RecordId {
    RecordId::new("scope", name.as_str())
}

/// Map a backend failure into the shared storage error.
pub(crate) fn db_err(source: surrealdb::Error) -> StorageError {
    StorageError::backend("surrealdb", source)
}

/// Failure while decoding a stored row.
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

/// Encode a 32-byte hash for a `SurrealDB` `bytes` field.
pub(crate) fn hash_bytes(hash: &[u8; 32]) -> Bytes {
    Bytes::from(hash.to_vec())
}

/// Encode a commit ID for a `SurrealDB` `bytes` field.
pub(crate) fn commit_bytes(id: CommitId) -> Bytes {
    hash_bytes(&id.0)
}

/// Decode a 32-byte hash value.
pub(crate) fn decode_hash(value: Value, column: &'static str) -> Result<[u8; 32], StorageError> {
    into_hash(value, column)
}

fn into_hash(value: Value, column: &'static str) -> Result<[u8; 32], StorageError> {
    match value {
        Value::Bytes(bytes) => bytes
            .into_inner()
            .to_vec()
            .try_into()
            .map_err(|bytes: Vec<u8>| StorageError::InvalidHashLength { len: bytes.len() }),
        _ => Err(corrupt(format!("{column} is not a byte string"))),
    }
}

/// Decode a commit ID value.
fn into_commit(value: Value, column: &'static str) -> Result<CommitId, StorageError> {
    into_hash(value, column).map(CommitId)
}

fn into_bytes(value: Value, column: &'static str) -> Result<Vec<u8>, StorageError> {
    match value {
        Value::Bytes(bytes) => Ok(bytes.into_inner().to_vec()),
        _ => Err(corrupt(format!("{column} is not a byte string"))),
    }
}

fn into_object(value: Value) -> Result<Object, StorageError> {
    match value {
        Value::Object(object) => Ok(object),
        _ => Err(corrupt("record is not an object")),
    }
}

fn take_field(object: &mut Object, column: &'static str) -> Result<Value, StorageError> {
    object
        .remove(column)
        .ok_or_else(|| corrupt(format!("record has no {column} field")))
}

fn take_optional_field(object: &mut Object, column: &'static str) -> Option<Value> {
    match object.remove(column) {
        Some(Value::None | Value::Null) | None => None,
        Some(value) => Some(value),
    }
}

pub(crate) fn take_optional_hash(
    object: &mut Object,
    column: &'static str,
) -> Result<Option<[u8; 32]>, StorageError> {
    take_optional_field(object, column)
        .map(|value| into_hash(value, column))
        .transpose()
}

fn take_i64(object: &mut Object, column: &'static str) -> Result<i64, StorageError> {
    match take_field(object, column)? {
        Value::Number(Number::Int(value)) => Ok(value),
        _ => Err(corrupt(format!("{column} is not an integer"))),
    }
}

fn take_i32(object: &mut Object, column: &'static str) -> Result<i32, StorageError> {
    let value = take_i64(object, column)?;
    i32::try_from(value).map_err(|_| StorageError::InvalidHistoryValue { column, value })
}

fn take_u64(object: &mut Object, column: &'static str) -> Result<u64, StorageError> {
    let value = take_i64(object, column)?;
    u64::try_from(value).map_err(|_| StorageError::InvalidHistoryValue { column, value })
}

fn take_bool(object: &mut Object, column: &'static str) -> Result<bool, StorageError> {
    match take_field(object, column)? {
        Value::Bool(value) => Ok(value),
        _ => Err(corrupt(format!("{column} is not a boolean"))),
    }
}

fn take_string(object: &mut Object, column: &'static str) -> Result<String, StorageError> {
    match take_field(object, column)? {
        Value::String(value) => Ok(value),
        _ => Err(corrupt(format!("{column} is not a string"))),
    }
}

fn take_array(object: &mut Object, column: &'static str) -> Result<Vec<Value>, StorageError> {
    match take_optional_field(object, column) {
        None => Ok(Vec::new()),
        Some(Value::Array(array)) => Ok(array.into_inner()),
        Some(_) => Err(corrupt(format!("{column} is not an array"))),
    }
}

/// A commit row as stored, including the local sequence.
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

pub(crate) fn decode_commit(value: Value) -> Result<StoredCommit, StorageError> {
    let mut object = into_object(value)?;
    let id = into_commit(take_field(&mut object, "hash")?, "hash")?;
    let seq = take_i64(&mut object, "seq")?;
    let created_at_ms = take_u64(&mut object, "created_at_ms")?;
    let parents = take_array(&mut object, "parents")?
        .into_iter()
        .map(|value| into_commit(value, "parents"))
        .collect::<Result<_, _>>()?;
    Ok(StoredCommit {
        id,
        seq,
        created_at_ms,
        parents,
    })
}

/// A chunk-history row as stored, including its record ID for rewrites.
pub(crate) struct StoredHistory {
    pub(crate) record: Option<RecordId>,
    pub(crate) entry: ChunkHistoryEntry,
}

pub(crate) fn decode_history(value: Value) -> Result<StoredHistory, StorageError> {
    let mut object = into_object(value)?;
    let record = match object.remove("id") {
        Some(Value::RecordId(record)) => Some(record),
        _ => None,
    };
    let commit = into_commit(take_field(&mut object, "commit")?, "commit")?;
    let dim = take_i32(&mut object, "dim")?;
    let kind_raw = take_i32(&mut object, "kind")?;
    let kind = RegionKind::new(kind_raw)
        .map_err(|_| StorageError::UnknownRegionKind { kind: kind_raw })?;
    let cx = take_i32(&mut object, "cx")?;
    let cz = take_i32(&mut object, "cz")?;
    let blob = take_optional_hash(&mut object, "blob")?.map(ChunkHash::new);
    Ok(StoredHistory {
        record,
        entry: ChunkHistoryEntry::new(
            ChunkCoord::new(Dimension::new(dim), kind, cx, cz),
            commit,
            blob,
        ),
    })
}

pub(crate) fn decode_commit_ref(value: Value) -> Result<CommitRef, StorageError> {
    let mut object = into_object(value)?;
    let name_raw = take_string(&mut object, "name")?;
    let name =
        RefName::parse(&name_raw).map_err(|_| StorageError::InvalidRefName { name: name_raw })?;
    let commit = take_optional_hash(&mut object, "commit")?.map(CommitId);
    let active = take_bool(&mut object, "active")?;
    let created_at_ms = take_u64(&mut object, "created_at_ms")?;
    Ok(CommitRef::new(name, commit, active, created_at_ms))
}

/// Decode a standalone integer value.
pub(crate) fn decode_int(value: &Value, column: &'static str) -> Result<i64, StorageError> {
    match value {
        Value::Number(Number::Int(value)) => Ok(*value),
        _ => Err(corrupt(format!("{column} is not an integer"))),
    }
}

/// Read just the creation time of a stored scope.
pub(crate) fn decode_created_at_ms(value: Value) -> Result<u64, StorageError> {
    let mut object = into_object(value)?;
    take_u64(&mut object, "created_at_ms")
}

pub(crate) fn decode_scope(value: Value) -> Result<NamedScope, StorageError> {
    let mut object = into_object(value)?;
    let name_raw = take_string(&mut object, "name")?;
    let name = ScopeName::parse(&name_raw)
        .map_err(|_| StorageError::InvalidScopeName { name: name_raw })?;
    let created_at_ms = take_u64(&mut object, "created_at_ms")?;
    let areas = take_array(&mut object, "areas")?
        .into_iter()
        .map(|value| {
            let mut area = into_object(value)?;
            let dim = take_i32(&mut area, "dim")?;
            let rect = Rect::new(
                take_i32(&mut area, "x0")?,
                take_i32(&mut area, "z0")?,
                take_i32(&mut area, "x1")?,
                take_i32(&mut area, "z1")?,
            );
            Ok((Dimension::new(dim), rect))
        })
        .collect::<Result<Vec<_>, StorageError>>()?;
    let display = name.as_str().to_owned();
    NamedScope::new(name, areas, created_at_ms)
        .map_err(|NamedScopeError::NoAreas| StorageError::EmptyScope { name: display })
}

/// Decode a blob payload value.
pub(crate) fn decode_payload(value: Value) -> Result<Vec<u8>, StorageError> {
    into_bytes(value, "payload")
}

/// Decode the coordinate columns of a chunk-history row.
pub(crate) fn decode_coord_fields(value: Value) -> Result<(i32, i32, i32, i32), StorageError> {
    let mut object = into_object(value)?;
    Ok((
        take_i32(&mut object, "dim")?,
        take_i32(&mut object, "kind")?,
        take_i32(&mut object, "cx")?,
        take_i32(&mut object, "cz")?,
    ))
}
