# ADR-0040: Backends are picked at the binary; `sekai-app` pins `WorldError`

- Status: Accepted
- Date: 2026-11-23

## Context

The store and world boundaries were shaped differently. Every store
store trait keeps an associated `Error`, `sekai-app` pins it to the
backend-neutral `StorageError` (`BlobStore<Error = StorageError>`), and a
backend boxes its native cause inside it, so `sekai-app` handles store
failures without knowing which backend runs, and the binary picks
`sekai-storage-local` itself. Each world tree also named its own error type
(ADR-0015), but `sekai-app` accepted any of them: every world operation
required `AppError: From<W::Error>`, and `AppError` carried the host tree's
`WorldError` as a variant. That variant was the reason `sekai-app` pulled in
`sekai-world-local` behind a default-on `backend-local` feature and
re-exported the host tree through its own root, so backend selection was
split across two layers.

## Decision

- The binary is the composition root: `sekai-cli` depends on
  `sekai-storage-local` and `sekai-world-local` directly. `sekai-app`
  names no concrete backend, has no `backend-local` feature, and no longer
  re-exports `HostWorldTree`, `HostRegion`, `HostWorldLock`, or
  `HostWorldLockError`.
- `sekai-world` owns `WorldError`, mirroring `StorageError`: a `no_std`
  enum with a hand-written `Display` whose variants are `Io` (a boxed
  tree-specific cause with its location), `Anvil`, `RegionFile`, `Locked`,
  `UnknownRegionPath`, and `AmbiguousRegion`. Locations are strings, since
  the boundary carries no paths.
- `WorldTree`, `RegionFile`, and `Region` keep their associated `Error`
  type, and `sekai-app` pins it the way it pins `StorageError`: its
  operations take `W: WorldTree<Error = WorldError>` and
  `R: RegionFile<Error = WorldError>`. The trait chain ties a tree's
  region handles and images to the tree's `Error`, so pinning the tree
  pins them too.
- `sekai-world-local` sets `Error = WorldError` and keeps no error enum of
  its own: it spells its paths into `WorldError` locations and boxes
  `std::io::Error` inside `WorldError::Io`.
- `AppError::World` wraps `sekai_world::WorldError` unconditionally, and
  the `AppError: From<W::Error>` bounds on the operations are gone.

## Consequences

- Store and world boundaries take the same shape - a trait-level
  associated `Error`, pinned to a backend-neutral type by `sekai-app` -
  and both backends are chosen in the same place. `app` depends on `core`, `anvil`, `world`, and
  `storage` only and names no host crate, which also keeps a later move of
  `app` to `no_std` free of backend edges.
- A tree that runs under `sekai-app` sets `Error = WorldError` and wraps
  its own causes in `WorldError::Io`, as a store backend wraps its causes
  in `StorageError`. A consumer other than `sekai-app` may still use a tree
  with its own error type (an in-memory tree can even name
  `core::convert::Infallible`), so ADR-0015's per-tree error type stands;
  only the conversion `sekai-app` accepted is narrowed to `WorldError`.
- For a `WorldError` tree, telling a missing file apart from another I/O
  failure takes a downcast of the boxed cause inside `WorldError::Io`.
- Breaking path change, acceptable pre-release: `sekai_app::{HostWorldTree,
  HostRegion, HostWorldLock, HostWorldLockError}` move to
  `sekai_world_local`, `sekai_world_local::WorldError` becomes
  `sekai_world::WorldError` (also re-exported as `sekai_app::WorldError`),
  and `WorldLocked { path }` becomes `Locked { location }`. Error messages
  and the CLI output contract are unchanged.
