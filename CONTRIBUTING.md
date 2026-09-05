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
cargo clippy --all-targets -- -D warnings

# Run all unit and integration tests
cargo test --all

```

## Testing Standards

* **Unit Tests**: Place in `src/` alongside the code. Ensure coverage for edge cases (corrupted headers, unexpected NBT structures, zero-length chunks).
* **Integration Tests**: Place in `tests/` directories within crates. Test atomic operations (e.g., MCA writes, rollbacks) using synthetic binary fixtures.

