//! SurrealDB-backed content-addressed blob store.

use sekai_core::{BlobStore, ChunkHash};
use sekai_storage::StorageError;
use surrealdb::Connection;

use crate::SurrealdbStore;
use crate::row::{BlobHashRow, BlobRow, IdRow, blob_record, db_err};

impl<C: Connection> SurrealdbStore<C> {
    async fn contains_blob(&self, hash: &ChunkHash) -> Result<bool, StorageError> {
        let row: Option<IdRow> = self.db.select(blob_record(hash)).await.map_err(db_err)?;
        Ok(row.is_some())
    }

    async fn fetch_blob(&self, hash: &ChunkHash, out: &mut Vec<u8>) -> Result<(), StorageError> {
        out.clear();
        let row: Option<BlobRow> = self.db.select(blob_record(hash)).await.map_err(db_err)?;
        let Some(row) = row else {
            return Err(StorageError::BlobMissing {
                hex: hash.hex_string(),
            });
        };
        let (_, payload) = row.into_parts()?;
        *out = payload;
        Ok(())
    }

    async fn put_blob(&self, hash: &ChunkHash, payload: &[u8]) -> Result<bool, StorageError> {
        let _write = self.write_lock.lock().await;
        let existing: Option<IdRow> = self.db.select(blob_record(hash)).await.map_err(db_err)?;
        if existing.is_some() {
            return Ok(false);
        }
        let created: Option<BlobRow> = self
            .db
            .create(blob_record(hash))
            .content(BlobRow::new(hash, payload))
            .await
            .map_err(db_err)?;
        Ok(created.is_some())
    }

    async fn remove_blob(&self, hash: &ChunkHash) -> Result<bool, StorageError> {
        let _write = self.write_lock.lock().await;
        let removed: Option<BlobRow> = self.db.delete(blob_record(hash)).await.map_err(db_err)?;
        Ok(removed.is_some())
    }

    async fn visit_blob_hashes<F>(&self, mut visit: F) -> Result<(), StorageError>
    where
        F: FnMut(&ChunkHash) -> bool + Send,
    {
        let mut response = self
            .db
            .query("SELECT hash FROM blob ORDER BY hash;")
            .await
            .map_err(db_err)?
            .check()
            .map_err(db_err)?;
        let rows: Vec<BlobHashRow> = response.take(0).map_err(db_err)?;
        for row in rows {
            if !visit(&row.into_hash()?) {
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
        // SurrealKV commits every `CREATE` durably; no batch barrier is needed.
        core::future::ready(Ok(()))
    }

    async fn visit_blobs<F>(&self, visit: F) -> Result<(), Self::Error>
    where
        F: FnMut(&ChunkHash) -> bool + Send,
    {
        self.visit_blob_hashes(visit).await
    }
}
