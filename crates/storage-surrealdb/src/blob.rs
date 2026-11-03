//! SurrealDB-backed content-addressed blob store.

use sekai_core::{BlobStore, ChunkHash};
use sekai_storage::StorageError;
use surrealdb::types::{Bytes, Value};

use crate::SurrealdbStore;
use crate::row::{blob_record, db_err, decode_hash, decode_payload};
use surrealdb::Connection;

impl<C: Connection> SurrealdbStore<C> {
    async fn contains_blob(&self, hash: &ChunkHash) -> Result<bool, StorageError> {
        let mut response = self
            .db
            .query("SELECT VALUE id FROM $record;")
            .bind(("record", blob_record(hash)))
            .await
            .map_err(db_err)?
            .check()
            .map_err(db_err)?;
        let rows: Vec<Value> = response.take(0).map_err(db_err)?;
        Ok(!rows.is_empty())
    }

    async fn fetch_blob(&self, hash: &ChunkHash, out: &mut Vec<u8>) -> Result<(), StorageError> {
        out.clear();
        let mut response = self
            .db
            .query("SELECT VALUE payload FROM $record;")
            .bind(("record", blob_record(hash)))
            .await
            .map_err(db_err)?
            .check()
            .map_err(db_err)?;
        let payloads: Vec<Value> = response.take(0).map_err(db_err)?;
        let Some(payload) = payloads.into_iter().next() else {
            return Err(StorageError::BlobMissing {
                hex: hash.hex_string(),
            });
        };
        *out = decode_payload(payload)?;
        Ok(())
    }

    async fn put_blob(&self, hash: &ChunkHash, payload: &[u8]) -> Result<bool, StorageError> {
        let _write = self.write_lock.lock().await;
        let mut response = self
            .db
            .query(
                "INSERT IGNORE INTO blob {
                     id: $record, hash: $hash, payload: $payload
                 }
                 RETURN AFTER;",
            )
            .bind(("record", blob_record(hash)))
            .bind(("hash", Bytes::from(hash.as_bytes().to_vec())))
            .bind(("payload", Bytes::from(payload.to_vec())))
            .await
            .map_err(db_err)?
            .check()
            .map_err(db_err)?;
        let inserted: Vec<Value> = response.take(0).map_err(db_err)?;
        Ok(!inserted.is_empty())
    }

    async fn remove_blob(&self, hash: &ChunkHash) -> Result<bool, StorageError> {
        let _write = self.write_lock.lock().await;
        let mut response = self
            .db
            .query("DELETE $record RETURN BEFORE;")
            .bind(("record", blob_record(hash)))
            .await
            .map_err(db_err)?
            .check()
            .map_err(db_err)?;
        let removed: Vec<Value> = response.take(0).map_err(db_err)?;
        Ok(!removed.is_empty())
    }

    async fn visit_blob_hashes<F>(&self, mut visit: F) -> Result<(), StorageError>
    where
        F: FnMut(&ChunkHash) -> bool + Send,
    {
        let mut response = self
            .db
            .query("SELECT VALUE hash FROM blob ORDER BY hash;")
            .await
            .map_err(db_err)?
            .check()
            .map_err(db_err)?;
        let hashes: Vec<Value> = response.take(0).map_err(db_err)?;
        for value in hashes {
            if !visit(&ChunkHash::new(decode_hash(value, "hash")?)) {
                break;
            }
        }
        Ok(())
    }
}

impl<C: Connection> BlobStore for SurrealdbStore<C> {
    type Error = StorageError;

    async fn contains(&self, hash: &ChunkHash) -> Result<bool, Self::Error> {
        self.contains_blob(hash).await
    }

    async fn fetch_into(&self, hash: &ChunkHash, out: &mut Vec<u8>) -> Result<(), Self::Error> {
        self.fetch_blob(hash, out).await
    }

    async fn put(&mut self, hash: &ChunkHash, payload: &[u8]) -> Result<bool, Self::Error> {
        self.put_blob(hash, payload).await
    }

    async fn remove(&mut self, hash: &ChunkHash) -> Result<bool, Self::Error> {
        self.remove_blob(hash).await
    }

    fn sync(&mut self) -> impl core::future::Future<Output = Result<(), Self::Error>> + Send {
        // SurrealKV commits every `INSERT` durably; no batch barrier is needed.
        core::future::ready(Ok(()))
    }

    async fn visit_blobs<F>(&self, visit: F) -> Result<(), Self::Error>
    where
        F: FnMut(&ChunkHash) -> bool + Send,
    {
        self.visit_blob_hashes(visit).await
    }
}
