# Project Context

This workspace contains a high-frequency, chunk-level deduplicated backup and rollback tool for Minecraft region files (`.mca`), written in Rust.

# Reference Documentation

Prior to generating, refactoring, or reviewing code, strictly follow the specifications in:
- **`ARCHITECTURE.md`**: System boundaries, core abstraction policy, 2-layer hashing, MVCC/GC data model, and safety invariants.
- **`CONTRIBUTING.md`**: Workspace crate structure, coding style, error handling, and testing guidelines.

# Code Generation Directives

- Respect crate boundaries inside `crates/*` and adhere strictly to the unidirectional dependency flow defined in `CONTRIBUTING.md`.
- Never use `unwrap()`, `expect()`, or `panic!` in non-test library code.
- Write code comments, documentation, and commit messages in English.


