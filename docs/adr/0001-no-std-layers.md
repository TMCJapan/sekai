# ADR-0001: `no_std` layered architecture

- Status: Accepted
- Date: 2026-09-01

## Context

Backup metadata, chunk hashing, and NBT handling must stay portable
(and testable without I/O) while orchestration needs threads, clocks,
and SQLite. A single-crate design would tangle pure policy with the
filesystem and the async runtime.

## Decision

`util`/`anvil`/`nbt`/`core` are `no_std` + `alloc` with unidirectional
dependencies (`app` → `core` → `{anvil, nbt}` → `util`); `storage`/
`world`/`app`/CLI own `std` + tokio. `core` also owns the async port
traits (`BlobStore`/`MetaStore`); backends implement them. Pluggability
lives at those two boundaries, not at reader/normalizer-style traits.

## Consequences

Pure crates lint clean for `thumbv7m`/`wasm32`. Filesystem, clocks,
parallelism, and timing stay in `world`/`storage`/`app`/CLI by
construction. New dependencies on pure crates need explicit
justification (see `CONTRIBUTING.md` allowlist).
