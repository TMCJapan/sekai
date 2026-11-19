# ADR-0035: Cut the core -> storage edge; storage owns the generic store algorithms

- Status: Accepted
- Date: 2026-11-19

## Context

ADR-0028 flipped the dependency edge so `core` depends on `storage`: the
use cases were generic over `MetaStore`, and the trait contract names the
canonical tree nodes, so the traits and the codec had to live below `core`.
That kept the policy portable, but it also made the pure-domain crate
depend on the storage boundary, and every store-touching algorithm (tree
merge/lookup/walk/diff, graph reads, ref/scope listings, commit planning
and recording) accumulated in `core::tree` and `core::usecase`.
`storage-local` then depended on `core` only to run those algorithms, so a
backend crate pulled in the whole policy layer.

## Decision

- `core` depends on `util` only. It keeps the util-type re-exports and the
  pure staging policy (`staging`: `Previous`, `Plan`, `Ingested`,
  `Assembled`, `stage_present`, `assemble`) - decisions with no store
  access, tested without I/O.
- `storage` gains the generic-over-`MetaStore` algorithms its trait
  contract needs: the tree merge/lookup/walk/diff operations and the
  region-hash helpers (`tree`), graph reads with
  `CommitError`/`ResolveError`/`CommitStats` (`graph`), ref reads (`refs`),
  scope reads (`scopes`), and the content-addressed `commit_hash`
  derivation (`meta`), which every backend must compute identically for
  `commit_tree` idempotency. The in-memory `MetaStore` fake moves to
  `storage::testing`, gated behind a `testing` feature that is off by
  default, so a normal build still holds no store implementation.
- `app` owns the store-touching orchestration only it runs: `plan_commit`,
  `record_commit` (+ `CommitReport`), and `plan_checkout` (+ `CheckoutPlan`,
  `CheckoutError`, `CheckoutReport`), beside the existing world ingest and
  checkout execution. `sekai_app` keeps re-exporting `CommitReport`,
  `CheckoutReport`, and `CommitStats`, so the CLI's paths are unchanged.
- `storage-local` depends on `storage` + `util` only. Its conformance suite
  stays trait-only: the checkout-plan flow is now a `visit_commit_chunks`
  walk, the same read `plan_checkout` consumes.

## Consequences

- The chain is `util <- storage <- {storage-local, app}` and
  `util <- core <- {world, world-local, app}`. `core` no longer names a
  store trait, and a backend crate no longer pulls in the policy layer.
- The generic algorithms sit beside the traits they implement, so a future
  backend reuses them - and the shared conformance flows - without new
  edges.
- Breaking path change, acceptable pre-release: `sekai_core::{usecase,
  tree, commit_hash, CommitReport, CheckoutReport, CommitError,
  ResolveError, ...}` no longer exist; the same items live at
  `sekai_storage` / `sekai_app` paths. No behavior, schema, or
  output-contract change.
- Supersedes the dependency-edge decision of ADR-0028; its trait and codec
  ownership stands. The commit-ID derivation of ADR-0020/ADR-0027 is
  unchanged, only relocated next to the trait contract that depends on it.
