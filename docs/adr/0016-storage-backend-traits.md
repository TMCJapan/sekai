# ADR-0016: Backend-neutral store traits and a separate SQLite crate

- Status: Accepted
- Date: 2026-10-21

## Context

`sekai-storage` mixed backend-neutral pieces (the file CAS, store locks, and
the async `BlobStore`/`MetaStore` ports re-exported from `core`) with the
SQLite implementation and sqlx. Consumers such as `sekai-app` named
`SqliteStore` directly, so another database, an in-memory backend, or a wasm
target could not be plugged in without branching inside the orchestration
layer.

## Decision

`sekai-storage` owns the high-level store boundary. `SekaiStore` combines a
metadata adapter and a blob adapter (ADR-0022 dropped the store-wide
exclusion lock and the `BlockingBlobStore` split); `SekaiStoreOpener` maps a
backend URL to an opened store. SQLite moves to the new
`sekai-storage-sqlite` crate,
which implements those traits together with the existing `core::port` traits.
`SekaiInstance<T: SekaiStore>` is generic over the store, with SQLite as the
feature-selected default.

## Consequences

- Adding mysql/postgres/embedded/in-memory backends means adding a sibling
  crate, not editing `core` or `app`.
- `sekai-storage` no longer depends on sqlx, so it can be compiled for
  targets without a native SQLite library. The current file CAS still uses
  `std::fs`; a wasm deployment will pair `sekai-storage` traits with a
  browser/in-memory blob backend.
- The store schema and `SCHEMA_VERSION` now live in
  `crates/storage-sqlite/`.
- The store error remains `StorageError`; backend-specific failures are
  wrapped in its `Backend` variant so every implementation shares one app
  error surface.
- The exclusion lock of ADR-0014 was a trait method (`SekaiStore::lock`)
  instead of a `FileCas` call, so a remote backend could supply its own
  mechanism. ADR-0022 removed it: exclusion is per world only, and nothing
  in the store is locked.
