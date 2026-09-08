# Project Structure

This project is organized as a Cargo workspace under `crates/`:

```text
.
└── crates
    ├── core        # Domain types, ports, and use cases (no_std, zero-dependency)
    ├── nbt         # Low-level NBT parser, decoder, and normalization views
    ├── mca         # MCA region files, world discovery, fingerprints, scans
    ├── storage     # CAS blob store, SQLite metadata, store assembly, hashing
    └── cli         # Composition root (library) and command-line binary

```

## Dependency Flow

Keep dependencies strictly unidirectional:
`cli` → `storage` / `mca` / `nbt` → `core`

The CLI assembles concrete adapters and executes `core` use cases; adapters
implement `core` ports. `core` depends on nothing.

## Safety Invariants

These hold for every change. The PR template asks for confirmation; explain
any exception in the PR body.

* **`core` purity**: `crates/core` stays `no_std` with zero external
  dependencies. I/O, SQLite, and NBT/codec work live behind `core` ports
  and are implemented in the outer crates.
* **Atomic file I/O**: never overwrite `.mca` files in place. Write to a
  temporary file in the same directory as the target and swap via atomic
  `rename`.
* **Captured-payload rollback**: store raw chunk payloads verbatim in CAS;
  normalization feeds diff views only, never stored blobs. Rollback
  reproduces the exact bytes captured for the snapshot, including volatile
  tags (e.g. `LastUpdate`) as of capture time — it rewinds them to the
  snapshot, it does not preserve their live values.
* **No server orchestration in libraries**: `save-off`/`save-all`, process
  control, and scheduling belong to the caller/CLI layer, never to library
  crates.
* **Rebuildable derived state**: caches like `region_state` are derived;
  wiping them must cost at most a full ingest on the next backup, never
  wrong data.
* **Two-phase destructive operations**: GC and similar destructive
  operations keep a dry-run/plan phase separate from apply/execution.

# Development Guidelines

## Tech Stack & Tooling

* **Rust Edition**: 2024
* **Core crate constraints**: `crates/core` must remain `no_std` with **zero external dependencies**.
* **Portability targets**: CI lints the library crates on every push and pull request.
* `no_std`: `core` must keep linting clean for `thumbv7m-none-eabi`.
* `wasm`: `core` and `nbt` must keep linting clean for `wasm32-unknown-unknown`. Both are pure computation (domain types, decoding, hashing), so they stay usable from a browser/wasm sandbox. `mca`, `storage` and `cli` own file I/O and SQLite and are intentionally **not** wasm targets.
* Neither target has a runner, so they are lint-only; the full workspace is linted **and** tested natively on Linux, macOS and Windows for both `x86_64` and `aarch64`.
* **Release artifacts**: CI builds `sekai-cli` in release mode on all six native platforms. Each binary is uploaded as a workflow artifact named `sekai-<target triple>`; because the upload step zips its input and drops file modes, the artifact wraps a `sekai-<target triple>.tar.gz` that keeps the binary executable.

## Coding Standards

* **Language**: Write all code comments, documentation, and commit messages in **English**.
* **Comments**: Focus on rationale ("why", design trade-offs, `unsafe` safety specifications) rather than explaining obvious implementation details.
* **Error Handling**:
* For `crates/core`, use core/custom error types (`core::fmt::Display` without external dependencies).
* For other library crates (`crates/*`), use `thiserror`.
* For `cli`, use `anyhow`.
* Do **NOT** use `panic!`, `unwrap()`, or `expect()` in non-test library code.


* **Performance Considerations**:
* Enforce zero-allocation on critical hot paths (reuse buffers, prefer stack allocations).
* Prefer static dispatch (generics) over dynamic dispatch (`dyn Trait`) in core performance paths.



# Pull Request Checklist

Before submitting a Pull Request, make sure your changes pass all checks:


```bash
# Format check
cargo fmt --all -- --check

# Lint check
cargo clippy --workspace --all-targets -- -D warnings

# Ensure crates/core remains no_std and dependency-free
cargo clippy -p sekai-core --target thumbv7m-none-eabi --no-default-features -- -D warnings

# Ensure the pure-computation crates keep linting clean for wasm
cargo clippy -p sekai-core -p sekai-nbt --target wasm32-unknown-unknown --no-default-features -- -D warnings

# Run all unit and integration tests
cargo test --workspace

# Run dependency, advisory, and license checks
cargo deny check

# Detect unused dependencies
cargo machete

# Build the shippable CLI binary
cargo build -p sekai-cli --release

```

## Testing Standards

* **Unit Tests**: Place in `src/` alongside the code. Ensure coverage for edge cases (corrupted headers, unexpected NBT structures, zero-length chunks).
* **Integration Tests**: Place in `tests/` directories within crates. Test atomic operations (e.g., MCA writes, rollbacks) using synthetic binary fixtures.

## Collaboration Workflow

File bugs, features, and performance reports with the issue templates in
`.github/ISSUE_TEMPLATE/` (English only). For backup-path performance
reports, include `backup --timing` output from a release build
(`--timing`/`--timing-json` only exist on `backup`; rollback/GC reports
describe reproduction and measured wall-clock instead); questions belong in
Discussions, not issues.

### Branching

Branch from `main` using one of these prefixes:

* `feature/<scope>`: new user-facing capability (e.g., `feature/gc-cli`).
* `perf/<scope>`: measured speedup with before/after numbers (`backup --timing`/`--timing-json` for backup-path changes, wall-clock + reproduction otherwise).
* `fix/<scope>`: bug correction.
* `refactor/<scope>`, `docs/<scope>`, `test/<scope>`: no behavior change.
* `build/<scope>`, `ci/<scope>`, `chore/<scope>`: tooling, CI, or routine maintenance without runtime behavior change.

### Commits

Write [Conventional Commits](https://www.conventionalcommits.org/):

* Format: `<type>(<scope>): <summary>` (e.g., `feat(storage): carry unchanged regions via INSERT ... SELECT`).
* Types: `feat`, `fix`, `perf`, `refactor`, `test`, `docs`, `build`, `ci`, `chore`.
* Scope is the crate or area (`core`, `nbt`, `mca`, `storage`, `cli`, `docs`, `ci`).
* Summaries are imperative, lowercase, without a trailing period.

### Storage Schema Changes

`meta.sqlite` is versioned with `PRAGMA user_version` (`SCHEMA_VERSION` in
`crates/storage/src/meta.rs`).

* Bump the version for any schema change and add a test pinning the version
  gate (old stores fail loudly with `UnsupportedSchema`). Fresh stores
  derive `user_version` from `SCHEMA_VERSION`, so bumping the constant is
  sufficient.
* The project is pre-release: old stores are recreated, not migrated. Never
  silently reinterpret an unknown version.
* Derived state (e.g., `region_state`) must degrade gracefully: wiping it may
  cost one slow backup, never correctness. Cover that path with a test.

