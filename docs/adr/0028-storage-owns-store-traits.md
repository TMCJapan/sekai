# ADR-0028: Storage owns the store traits

- Status: Accepted
- Date: 2026-11-15

## Context

ADR-0001 placed the async `BlobStore`/`MetaStore` traits in `core::port`,
and ADR-0016 had `sekai-storage` re-export them from its `api` module.
That split the ownership of the storage boundary: `storage` is the
backend-neutral boundary crate, yet the traits it is built around lived in
`core`, and `storage` depended on `core` only to name them. The `port`
module name also obscured what the module holds.

The traits cannot simply move while the dependency edges stay put: `core`
use cases are generic over `MetaStore`, and `MetaStore`'s contract names
the canonical tree node types (`RootNode`, `DimNode`, `RegionNode`,
`TreeNodes`), so the traits and the tree representation must live in a
crate that `core` can depend on without a cycle.

## Decision

- The traits move to `sekai-storage` as the flat `blob` and `meta`
  modules; the `port` wrapper is retired. `storage` re-exports
  `BlobStore`/`MetaStore` at its root (and keeps the `api` re-exports).
- The canonical snapshot-tree codec (node types, entries, `TreeNodes`,
  `TreeError`, `NodeLevel`, `empty_tree`) moves from `core::tree` to
  `storage::tree`: the stored byte format is part of the storage contract.
  `core::tree` keeps the generic merge/lookup/diff operations over it.
- The dependency edge flips: `storage` depends on `util` only; `core`
  depends on `storage` (and `util`). The chain is
  `util <- storage <- core <- app`.
- Downstream imports name `sekai_storage` directly for the traits and tree
  types; `core` re-exports only util types and its own policy API. Backend
  crates implement `sekai_storage::MetaStore`/`BlobStore`.

## Consequences

- `sekai-storage` is self-contained: a new backend (mysql, postgres,
  in-memory, wasm) implements its traits without touching `core`, and the
  crate stays `no_std` + `alloc` on the `thumbv7m`/`wasm32` gates.
- `core` is purely the use-case policy layer over the storage model; the
  tree codec has one home beside the traits that exchange it.
- Breaking path change, acceptable pre-release: `sekai_core::{BlobStore,
  MetaStore, RootNode, TreeError, ...}` and `sekai_core::port` no longer
  exist; the same types live at `sekai_storage` paths. No behavior,
  schema, or output-contract change.
- The port-ownership statements of ADR-0001 and ADR-0016 are superseded;
  the rest of both decisions (layer split, backend-neutral boundary,
  sibling backend crates) stands.
