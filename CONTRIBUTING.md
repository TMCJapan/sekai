# Project Structure

This project is organized as a Cargo workspace under `crates/`:

```text
.
└── crates
    ├── util            # Shared value types: coordinates, hashes, history records (no_std, zero dependencies)
    ├── anvil           # Pure Anvil sector codec + decompression (no_std + alloc, ex-mca)
    ├── nbt             # Pure NBT parse/diff views over raw NBT bytes (no_std + alloc, hand-rolled parser)
    ├── core            # Use-case policy API over shared types (no_std + alloc, depends on util/anvil/nbt)
    ├── storage         # Single crate (std + tokio): async traits (api), file CAS (cas),
    │                   # feature-gated DB backends (sqlite ships; mysql/postgres reserved stubs)
    ├── world           # Filesystem owner: discovery, fingerprints, scans, atomic swaps (std)
    ├── app             # Composition/orchestration library for third parties (std + tokio)
    └── cli             # Thin clap binary over sekai-app (package sekai-cli, binary sekai, no logic)
```

## Dependency Flow

Keep dependencies strictly unidirectional (inward):

```text
sekai (bin) -> app -> {core, world, storage}
app -> core -> {anvil, nbt}
world -> {anvil, core}
storage -> core (whose `port` module owns the async traits; backend modules implement them)
{anvil, nbt, core} -> util (world/storage/app reach shared types through `core` re-exports)
```

Rules:

* `core` never depends on `world`, `storage`, `app`, or the binary.
  `world` already depends on `core` types, so the reverse edge would be a
  Cargo package cycle.
* `util` is a dependency leaf: zero external dependencies, pure data plus
  total validation only (no hashing backends, no codecs, no ports, no
  orchestration). `anvil`/`nbt` name util types directly (`Dimension`
  custom ids, `DiffHash` digests); `core` composes them and re-exports
  every util type at its root so downstream paths stay stable.
* `anvil` never calls `nbt`. `core`/`app` drives `anvil`'s decompressed raw
  NBT bytes into `nbt` (opt-in diff path), keeping the pure DAG acyclic and
  the backup hot path decode-free. Compression framing is MCA spec and lives
  in `anvil`; `nbt` never sees a compression-type byte.
* `async`/tokio lives in `storage`, `world`, `app`, and the binary only.
  `anvil`/`nbt` are sync-only; `core` policy is sync except for `async fn`
  in traits (RPITIT, see below).
* Inside `storage`, keep module boundaries strict (`api` / `cas` /
  `sqlite` / `mysql` / `postgres`) so a future crate split stays
  mechanical. Backend modules must not reach into each other.

## Safety Invariants

These hold for every change. The PR template asks for confirmation; explain
any exception in the PR body.

* **Pure-layer hygiene**: `util`, `anvil`, `nbt`, and `core` stay `no_std` + `alloc`.
  No `std::fs`, `Path`, `std::time`, `std::io::Error`, or `thiserror` there;
  hand-written `core::fmt::Display` error enums only. No `tokio`, no
  `async-trait` macro.
* **`core` is an API layer and the port hub**: it composes the concrete
  pure crates (`anvil`, `nbt`) over shared `util` types for sector/diff
  logic, re-exports every util type at its root, and owns the async
  `BlobStore`/`MetaStore` ports (`port` module, RPITIT). Pluggability lives
  at those ports (implemented by `storage` backends) and at the `world`
  observation boundary - not at `RegionReader`/`Normalizer`-style traits.
* **`core` never touches the filesystem**: `world` produces `Observation` /
  `RegionFingerprint`; `app` feeds them into `core::plan_*`.
* **Atomic file I/O**: never overwrite `.mca` files in place. `anvil` builds
  the `Vec<u8>` image; `world::atomic_swap` writes it to a temporary file
  in the same directory as the target and swaps via atomic `rename`.
* **Captured-payload rollback**: store raw chunk payloads verbatim in CAS;
  normalization feeds diff views only, never stored blobs. Rollback
  reproduces the exact bytes captured for the snapshot, including volatile
  tags (e.g. `LastUpdate`) as of capture time - it rewinds them to the
  snapshot, it does not preserve their live values.
* **No server orchestration in libraries**: `save-off`/`save-all`, process
  control, and scheduling belong to the caller, never to library crates.
* **Rebuildable derived state**: caches like `region_state` are derived;
  wiping them must cost at most a full ingest on the next backup, never
  wrong data.
* **Two-phase destructive operations**: GC and similar destructive
  operations keep a dry-run/plan phase separate from apply/execution.
* **Crash order**: CAS blobs flushed + synced *before* DB metadata commit;
  GC unlinks *after* metadata commit. The barrier calls live at the `app`
  call site, never hidden inside adapters.

# Development Guidelines

## Tech Stack & Tooling

* **Rust Edition**: 2024 (toolchain pinned in `rust-toolchain.toml`)
* **Pure crates**: `util`, `anvil`, `nbt`, `core` must remain `no_std` + `alloc`
  with no `std`-only or async-runtime dependencies.
* **Pure-crate dependency allowlist** (keep this list minimal and
  `no_std`-gated):
  * `util`: nothing. New dependencies need explicit justification.
  * `anvil`: `sekai-util` (shared dimension codes), `miniz_oxide` (zlib/raw-inflate), `crc32fast` (gzip footer),
    `lz4_flex` block API (lz4-java stream bodies), `twox-hash` (lz4-java
    block checksums), `blake3` (header/dimension digests, all with
    `default-features = false`).
  * `nbt`: `sekai-util` (shared `DiffHash`) plus `blake3` with
    `default-features = false` for the canonical digest. No `serde`:
    the `Value` data model has deliberately no `Serialize` impl
    (derived output would be externally tagged, competing with the
    SNBT `Display` that machine-readable output uses).
  * `core`: `sekai-util` (shared types, re-exported at the root) plus
    `anvil` + `nbt` (concrete composition targets) plus `blake3` with
    `default-features = false` for hashing.
* **Async policy**: `async fn` in traits via RPITIT (return-position
  `impl Future`, no `async-trait` macro, no boxing). All trait futures are
  `Send`-bounded for tokio. Static dispatch (`generics`) is preferred over
  `dyn Trait`; if `dyn` is ever needed, box at the outer (`app`) layer, not
  in `core`.
* **Runtime**: tokio in `storage`, `world` (I/O parts), `app`, and the
  binary only.
* **Portability targets**: CI lints the pure crates on every push and pull request.
* `no_std`: `util`, `anvil`, `nbt`, `core` must keep linting clean for `thumbv7m-none-eabi`.
* `wasm`: `util`, `anvil`, `nbt`, `core` must keep linting clean for `wasm32-unknown-unknown`.
  `world`, `storage`, `app`, and the binary own file I/O, sockets, and
  SQLite and are intentionally **not** wasm targets.
* Neither target has a runner, so they are lint-only; the full workspace is
  linted **and** tested natively on Linux, macOS and Windows for both `x86_64`
  and `aarch64`.
* **Release artifacts**: CI builds `sekai` in release mode on all six native
  platforms. Each binary is uploaded as a workflow artifact named
  `sekai-<target triple>`; because the upload step zips its input and drops
  file modes, the artifact wraps a `sekai-<target triple>.tar.gz` that keeps
  the binary executable.
* **Storage backends**: this project currently ships SQLite only, as the `sqlite`
  module of the single `sekai-storage` crate (via sqlx, feature
  `backend-sqlite`, default-on). `mysql` / `postgres` modules are reserved
  stubs behind `backend-mysql` / `backend-postgres` features.
  Backend selection is feature-gated at compile time plus URL-selected at
  runtime (`sqlite://...`, `mysql://...`, `postgres://...`); requesting an
  uncompiled backend is a loud `UnsupportedBackend` error.

## Coding Standards

* **Language**: Write all code comments, documentation, and commit messages in **English**.
* **Comments**: Focus on rationale ("why", design trade-offs, `unsafe` safety specifications) rather than explaining obvious implementation details.
* **Error Handling**:
* For pure crates (`util`, `anvil`, `nbt`, `core`), use hand-written error types
  (`core::fmt::Display` without external dependencies).
* For `std` library crates (`storage`, `world`, `app`), use `thiserror`.
* For the binary, use `anyhow`.
* Do **NOT** use `panic!`, `unwrap()`, or `expect()` in non-test library code.

* **Performance Considerations**:
* Enforce zero-allocation on critical hot paths (reuse buffers, prefer stack allocations).
* Prefer static dispatch (generics) over dynamic dispatch (`dyn Trait`) in core performance paths.
* Keep the backup hot path decode-free: `nbt` runs only on the opt-in
  (`with_diff`) path, never per-chunk by default.

# Pull Request Checklist

Before submitting a Pull Request, make sure your changes pass all checks:


```bash
# Format check
cargo fmt --all -- --check

# Lint check
cargo clippy --workspace --all-targets -- -D warnings

# Ensure the pure crates remain no_std
cargo clippy -p sekai-util -p sekai-anvil -p sekai-nbt -p sekai-core --target thumbv7m-none-eabi -- -D warnings

# Ensure the pure crates keep linting clean for wasm
cargo clippy -p sekai-util -p sekai-anvil -p sekai-nbt -p sekai-core --target wasm32-unknown-unknown -- -D warnings

# Run all unit and integration tests (default backend)
cargo test --workspace

# Backend matrix (when touching storage backends)
cargo test -p sekai-storage
cargo test -p sekai-storage --no-default-features --features backend-sqlite
# mysql/postgres: reserved; run once implemented, e.g.
# cargo test -p sekai-storage --no-default-features --features backend-mysql

# Run dependency, advisory, and license checks
cargo deny check

# Detect unused dependencies
cargo machete

# Build the shippable binary
cargo build -p sekai-cli --release

```

## Testing Standards

* **Unit Tests**: Place in `src/` alongside the code. Ensure coverage for edge cases (corrupted headers, unexpected NBT structures, zero-length chunks).
* **Integration Tests**: Place in `tests/` directories within crates. Test atomic operations (e.g., MCA writes, rollbacks) using synthetic binary fixtures. The `test-world/` corpus (small real-data region files, see its README for provenance) is the exception: copy it into a temp dir first, never mutate it in place. Crash-order tests (torn backup leaves orphans, never dangling references) are mandatory for `app`/`world`/`storage` changes.
* **Backend tests**: the `sqlite` module must cover the version gate
  (`UnsupportedSchema`), the incremental-carry path, and derived-state
  wipe recovery. Future backend modules must run the same suite via the
  shared `api` conformance tests.
* **Benchmarks**: micro benches live beside their crates (`cargo bench -p
  sekai-anvil -p sekai-nbt -p sekai-core`); integration flows in
  `crates/app/benches` (seeded worlds from `benches/common.rs`, `cargo
  bench -p sekai-app --bench backup`). Record new baselines in
  `docs/benchmarks.md` when a `perf` change lands. The `bench.yaml`
  workflow runs on schedule only, never as a merge gate.

## Collaboration Workflow

File bugs, features, and performance reports with the issue templates in
`.github/ISSUE_TEMPLATE/` (English only). Performance reports must include
the affected command's `--timing` output from a release build
(`--timing` exists on `backup`/`rollback`/`gc`/`diff`/`debug scan`;
for backup-path slowdowns prefer `--timing --json`, which alone carries
the per-region breakdown); bug reports include it only for speed aspects,
never for pure correctness bugs; questions belong in
Discussions, not issues.

### Branching

Branch from `main` using one of these prefixes:

* `feature/<scope>`: new user-facing capability (e.g., `feature/gc-cli`).
* `perf/<scope>`: measured speedup with before/after numbers (affected command's `--timing`; release build, reproduction steps included).
* `fix/<scope>`: bug correction.
* `refactor/<scope>`, `docs/<scope>`, `test/<scope>`: no behavior change.
* `build/<scope>`, `ci/<scope>`, `chore/<scope>`: tooling, CI, or routine maintenance without runtime behavior change.

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

* Format: `<type>(<scope>): <summary>` (e.g., `feat(storage): count carried chunks via effective-rows query`).
* Types: `feat`, `fix`, `perf`, `refactor`, `test`, `docs`, `build`, `ci`, `chore`.
* Scope is the crate or area (`core`, `nbt`, `anvil`, `world`, `app`, `storage`, `cli`, `docs`, `ci`).
* Summaries are imperative, lowercase, without a trailing period.

### Storage Schema Changes

The metadata schema is shared across backends and versioned
(`SCHEMA_VERSION` in `crates/storage/src/sqlite.rs`).

* Bump the version for any schema change and add a test pinning the version
  gate (old stores fail loudly with `UnsupportedSchema`). Fresh stores
  derive the version from `SCHEMA_VERSION`, so bumping the constant is
  sufficient.
* The project is pre-release: old stores are recreated, not migrated. Never
  silently reinterpret an unknown version.
* Derived state (e.g., `region_state`) must degrade gracefully: wiping it may
  cost one slow backup, never correctness. Cover that path with a test.
