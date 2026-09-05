# Project Structure

This project is organized as a Cargo workspace under `crates/`:

```text
.
└── crates
    ├── core        # Pure domain models, types, and traits (no_std, zero-dependency)
    ├── nbt         # Low-level NBT parser, decoder, and normalization views
    ├── mca         # MCA region file reader/writer and sector management
    ├── storage     # CAS blob store and SQLite metadata persistence
    ├── engine      # Backup, rollback, diffing, and GC orchestration
    └── cli         # Command-line interface binary

```

## Dependency Flow

Keep dependencies strictly unidirectional:
`cli` → `engine` → `storage` / `mca` / `nbt` → `core`

# Development Guidelines

## Tech Stack & Tooling

* **Rust Edition**: 2024
* **Core crate constraints**: `crates/core` must remain `no_std` with **zero external dependencies**.
* **Portability targets**: CI lints the library crates on every push and pull request.
* `no_std`: `core` must keep linting clean for `thumbv7m-none-eabi`.
* `wasm`: `core` and `nbt` must keep linting clean for `wasm32-unknown-unknown`. Both are pure computation (domain types, decoding, hashing), so they stay usable from a browser/wasm sandbox. `mca`, `storage`, `engine` and `cli` own file I/O and SQLite and are intentionally **not** wasm targets.
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

