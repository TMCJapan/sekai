# Project Structure

This project is organized as a Cargo workspace under `crates/`:

```text
.
└── crates
    ├── util            # Shared value types: coordinates, hashes, snapshot records (no_std, zero dependencies)
    ├── anvil           # Pure Anvil sector codec, decompression, and normalized chunk hashing (no_std + alloc, ex-mca)
    ├── core            # Use-case policy API over shared types (no_std + alloc, depends on util)
    ├── storage         # Backend-neutral store traits and locks (std + tokio)
    ├── storage-local   # Local backend: SQLite metadata (sqlx) + sled blobs (std + tokio)
    ├── world           # World access boundary (no_std + alloc): WorldTree trait,
    │                   # RegionFile handle, WorldLock primitives
    ├── world-local     # Host filesystem tree (std): HostWorldTree, layout detection,
    │                   # region handles, the world file lock, atomic swaps
    ├── app             # Composition/orchestration library for third parties (std + tokio)
    └── cli             # Thin clap binary over sekai-app (package sekai-cli, binary sekai, no logic)
```

## Dependency Flow

Keep dependencies strictly unidirectional (inward):

```text
sekai (bin) -> app -> {core, world, world-local, storage, storage-local}
app -> {core, anvil}
world -> {anvil, core}
world-local -> {world, anvil, core}
storage -> core (whose `port` module owns the async traits; backend crates implement them)
storage-local -> storage
{anvil, core} -> util (world/world-local/storage/app reach shared types through `core` re-exports)
```

Rules:

- `core` never depends on `world`, `storage`, `app`, or the binary.
  `world` already depends on `core` types, so the reverse edge would be a
  Cargo package cycle.
- `util` is a dependency leaf: pure data plus total validation only, plus
  the shared Blake3 primitive (`hash_bytes`, `BytesHasher`). It owns no
  codecs, ports, or orchestration. `anvil` names util types directly
  (`Dimension` custom ids, `RegionKind`); `core` re-exports every util type
  at its root so downstream paths stay stable, and `app` composes `core`
  policy with the `anvil` codec.
- NBT parsing and normalized views are private to `anvil`. `app` calls
  `Chunk::normalized_hash()` and never sees raw NBT types, so the
  parser cannot drift into a second public path. Compression framing and
  chunk schema knowledge both live in `anvil`.
- `async`/tokio lives in `storage`, `storage-local`, `app`, and the
  binary only. `anvil` is sync-only; `core` policy is sync except for
  `async fn` in traits (RPITIT, see below).
- `storage` owns backend-neutral boundaries (`api` / `store`);
  concrete database backends live in sibling crates such as
  `storage-local` and only communicate through the exported traits.
  Backend crates must not reach into each other.

## Safety Invariants

These hold for every change. The PR template asks for confirmation; explain
any exception in the PR body.

- **Pure-layer hygiene**: `util`, `anvil`, `core`, and `world` stay
  `no_std` + `alloc`.
  No `std::fs`, `Path`, `std::time`, `std::io::Error`, or `thiserror` there;
  hand-written `core::fmt::Display` error enums only. No `tokio`, no
  `async-trait` macro.
- **`core` is an API layer and the port hub**: it owns the use-case policy
  over shared `util` types, re-exports every util type at its root, and
  owns the async `BlobStore`/`MetaStore` ports (`port` module, RPITIT).
  `app` composes it with the `anvil` codec for payload digests and diffs.
  Pluggability lives at those ports (implemented by `storage` backends)
  and at the `world` observation boundary - not at
  `RegionReader`/`Normalizer`-style traits.
- **`core` never touches the filesystem**: `world-local` discovers the region
  files; `app` feeds their `RegionKey`s into `core::plan_*`.
- **The world boundary is path-free**: the operations receive `RegionFile`
  handles (read, size, fingerprint, write, remove) and never a `Path`; only
  `sekai-world-local` knows where a region file lives. A tree outside this
  workspace implements `WorldTree` plus one handle
  ([ADR-0015](docs/adr/0015-world-region-handles.md)).
- **Atomic file I/O**: never overwrite `.mca` files in place. `anvil` builds
  the `Vec<u8>` image; `sekai-world-local::atomic_swap` writes it to a temporary file
  in the same directory as the target and swaps via atomic `rename`.
- **Captured-payload checkout**: store raw chunk payloads verbatim in CAS.
  A chunk's CAS key is its `Chunk::normalized_hash` digest, which is also
  what `diff` compares; nothing else hashes a chunk. A payload is stored
  only when it yields that key: a `region/` payload must decode as a level
  chunk, and an `entities`/`poi` payload must be well-formed NBT. Undecodable
  or unsupported payloads fail ingest; there is no raw-byte fallback.
  Checkout reproduces the exact bytes captured for the commit, including
  volatile tags (e.g. `LastUpdate`) as of capture time - it rewinds them to
  the commit, it does not preserve their live values.
- **No server orchestration in libraries**: `save-off`/`save-all`, process
  control, and scheduling belong to the caller, never to library crates.
- **No persisted derived state**: commit decisions compare chunk hashes
  through the snapshot tree, so the schema holds no cache that can drift
  from the source of truth.
- **Canonical snapshot trees**: every tree node is stored as the exact
  sorted entry array its hash covers, and a node may name only the level
  below it. Empty child nodes are never written; absence is the tombstone.
- **Two-phase destructive operations**: checkout and similar destructive
  operations keep a plan phase separate from apply/execution.
- **Crash order**: CAS blobs flushed + synced _before_ DB metadata commit,
  so a torn commit leaves at most orphan blobs, never dangling references.
  The barrier calls live at the `app` call site, never hidden inside
  adapters.

# Development Guidelines

## Tech Stack & Tooling

- **Rust Edition**: 2024 (toolchain pinned in `rust-toolchain.toml`)
- **Pure crates**: `util`, `anvil`, `core`, and `world` must remain
  `no_std` + `alloc` with no `std`-only or async-runtime dependencies.
- **Pure-crate dependency allowlist** (keep this list minimal and
  `no_std`-gated):
    - `util`: `blake3` with `default-features = false` for the shared digest
      primitive. Other new dependencies need explicit justification.
    - `anvil`: `sekai-util` (shared dimension and region-kind codes),
      `miniz_oxide` (zlib/raw-inflate), `crc32fast` (gzip footer),
      `lz4_flex` block API (lz4-java stream bodies), `twox-hash` (lz4-java
      block checksums), and `nanonbt` (chunk NBT parsing and normalized
      views; private to the crate).
    - `core`: `sekai-util` (shared types, re-exported at the root).
    - `world`: `sekai-anvil` (types only) plus `sekai-core`.
- **Async policy**: `async fn` in traits via RPITIT (return-position
  `impl Future`, no `async-trait` macro, no boxing). All trait futures are
  `Send`-bounded for tokio. Static dispatch (`generics`) is preferred over
  `dyn Trait`; if `dyn` is ever needed, box at the outer (`app`) layer, not
  in `core`.
- **Runtime**: tokio in `storage`, `storage-local`, `app`, and the binary
  only; `world` and `world-local` are sync (blocking file I/O lives in
  `world-local`).
- **Portability targets**: CI lints the pure crates on every push and pull request.
- `no_std`: `util`, `anvil`, `core`, and `world` must keep linting clean
  for `thumbv7m-none-eabi`.
- `wasm`: `util`, `anvil`, `core`, `world`, and the backend-neutral
  `storage` crate must keep linting clean for `wasm32-unknown-unknown`.
  `world-local`, `storage-local`, `app`, and the binary own host file I/O, locks,
  and the databases (SQLite, sled) and are intentionally **not** wasm targets yet. The
  backend-neutral store traits exist so a wasm backend can be added as a
  sibling crate without touching `core`/`app`.
- Neither target has a runner, so they are lint-only; the full workspace is
  linted **and** tested natively on Linux, macOS and Windows for both `x86_64`
  and `aarch64`.
- **Release artifacts**: CI builds `sekai` in release mode on all six native
  platforms. Each binary is uploaded as a workflow artifact named
  `sekai-<target triple>`; because the upload step zips its input and drops
  file modes, the artifact wraps a `sekai-<target triple>.tar.gz` that keeps
  the binary executable.
- **Storage backends**: this project ships one local backend, in the
  `sekai-storage-local` crate (SQLite metadata via sqlx, blobs via sled;
  app feature `backend-local`, default-on). `sekai-storage` stays
  backend-neutral and owns the traits and locks; it holds no backend
  modules, so a mysql or postgres backend arrives as a sibling crate
  rather than as a stub.
  Backend selection is compile-time via the `backend-local` feature;
  `--store` names the local store's root directory directly. A build
  without `backend-local` has no store-opening entry point at all.

## Coding Standards

- **Language**: Write all code comments, documentation, and commit messages in **English**.
- **Comments**: Focus on rationale ("why", design trade-offs, `unsafe` safety specifications) rather than explaining obvious implementation details.
- **Error Handling**:
- For pure crates (`util`, `anvil`, `core`, `world`), use hand-written error types
  (`core::fmt::Display` without external dependencies).
- For `std` library crates (`storage`, `world-local`, `app`), use `thiserror`.
- For the binary, use `anyhow`.
- Do **NOT** use `panic!`, `unwrap()`, or `expect()` in non-test library code.

- **Performance Considerations**:
- Enforce zero-allocation on critical hot paths (reuse buffers, prefer stack allocations).
- Prefer static dispatch (generics) over dynamic dispatch (`dyn Trait`) in core performance paths.
- Keep ingest's per-chunk work bounded: decompress and normalize once per
  in-scope chunk, reuse buffers, and never re-read a payload the CAS write
  already holds.

# Pull Request Checklist

Before submitting a Pull Request, make sure your changes pass all checks:

```bash
# Format check
cargo fmt --all -- --check

# Lint check
cargo clippy --workspace --all-targets -- -D warnings

# Ensure the pure crates remain no_std
cargo clippy -p sekai-util -p sekai-anvil -p sekai-core -p sekai-world --target thumbv7m-none-eabi -- -D warnings

# Ensure the pure crates and backend-neutral storage stay lint-clean for wasm
cargo clippy -p sekai-util -p sekai-anvil -p sekai-core -p sekai-world -p sekai-storage --target wasm32-unknown-unknown --no-default-features -- -D warnings

# Run all unit and integration tests (default backend)
cargo nextest run --workspace

# Feature-combination lint: default, none, each backend, all
cargo hack --each-feature clippy --workspace --all-targets -- -D warnings

# Feature-combination tests: the store-backed suites need `backend-local`,
# so they are compiled out of the other combinations
cargo hack --each-feature nextest run --workspace --all-targets

# Backend matrix (when touching storage backends)
cargo nextest run -p sekai-storage -p sekai-storage-local
cargo nextest run -p sekai-app --no-default-features --features backend-local

# Run dependency, advisory, and license checks
cargo deny check

# Detect unused dependencies
cargo machete

# Build the shippable binary
cargo build -p sekai-cli --release

```

## Testing Standards

- **Unit Tests**: Place in `src/` alongside the code. Ensure coverage for edge cases (corrupted headers, unexpected NBT structures, zero-length chunks).
- **Integration Tests**: Place in `tests/` directories within crates. Test atomic operations (e.g., MCA writes, checkouts) using synthetic binary fixtures. The `test-world/` corpus (small real-data region files, see its README for provenance) is the exception: copy it into a temp dir first, never mutate it in place. Crash-order tests (torn commit leaves orphans, never dangling references) are mandatory for `app`/`world-local`/`storage` changes.
- **Backend tests**: `sekai-storage-local` must cover both version gates
  (`UnsupportedSchema`), snapshot-tree path reads and shared-subtree reuse,
  and the no-derived-state recovery guarantees. Future backend crates must
  run the same suite via the shared conformance flows.
- **Benchmarks**: micro benches live beside their crates (`cargo bench -p
sekai-anvil -p sekai-core`); integration flows in
  `crates/app/benches` (seeded worlds from `benches/common.rs`, `cargo
bench -p sekai-app --bench commit`). Record new baselines in
  `docs/benchmarks.md` when a `perf` change lands. The `bench.yaml`
  workflow runs on schedule only, never as a merge gate.

## Collaboration Workflow

File bugs, features, and performance reports with the issue templates in
`.github/ISSUE_TEMPLATE/` (English only). Performance reports must include
the affected command's `--timing` output from a release build
(`--timing` exists on every command except `log`;
for commit-path slowdowns prefer `--timing --json`, which alone carries
the per-region breakdown); bug reports include it only for speed aspects,
never for pure correctness bugs; questions belong in
Discussions, not issues.

### Branching

Branch from `main` using one of these prefixes:

- `feature/<scope>`: new user-facing capability (e.g., `feature/scope-cli`).
- `perf/<scope>`: measured speedup with before/after numbers (affected command's `--timing`; release build, reproduction steps included).
- `fix/<scope>`: bug correction.
- `refactor/<scope>`, `docs/<scope>`, `test/<scope>`: no behavior change.
- `build/<scope>`, `ci/<scope>`, `chore/<scope>`: tooling, CI, or routine maintenance without runtime behavior change.

### Architecture Decision Records

Record lasting design decisions in `docs/adr/` as `NNNN-kebab-case-title.md`
(start from `template.md`). Required for: new dependencies, schema
changes, output-contract changes, and performance-relevant architecture
calls. Routine refactors, bug fixes, and test-only changes do not need
one. Each ADR has Status (`Proposed`/`Accepted`/`Superseded`), Context,
Decision, and Consequences; keep it short and link follow-ups instead
of expanding scope.

### Commits

Write [Conventional Commits](https://www.conventionalcommits.org/):

- Format: `<type>(<scope>): <summary>` (e.g., `feat(storage): count carried chunks via effective-rows query`).
- Types: `feat`, `fix`, `perf`, `refactor`, `test`, `docs`, `build`, `ci`, `chore`.
- Scope is the crate or area (`core`, `anvil`, `world`, `world-local`, `app`, `storage`, `cli`, `docs`, `ci`).
- Summaries are imperative, lowercase, without a trailing period.

### Storage Schema Changes

The metadata schema is backend-owned and versioned
(`SCHEMA_VERSION` in `crates/storage-local/src/meta.rs`); the blob keyspace
has its own gate (`SCHEMA_VERSION` in `crates/storage-local/src/blob.rs`).

- Bump the version for any schema change and add a test pinning the version
  gate (old stores fail loudly with `UnsupportedSchema`). Fresh stores
  derive the version from `SCHEMA_VERSION`, so bumping the constant is
  sufficient.
- The project is pre-release: old stores are recreated, not migrated. Never
  silently reinterpret an unknown version.
- The schema keeps only source-of-truth history and snapshot trees. Derived
  data (fingerprints, cached counts) stays out of the database so no rebuild
  path can drift; cover commit decisions with tests against the tree tables
  themselves.
