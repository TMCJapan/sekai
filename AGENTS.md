# Project Context

This workspace contains a high-frequency, chunk-level deduplicated backup and rollback tool for Minecraft region files (`.mca`), written in Rust.

# Reference Documentation

Prior to generating, refactoring, or reviewing code, strictly follow the specifications in:
- **`ARCHITECTURE.md`**: System boundaries, core abstraction policy, 2-layer hashing, MVCC/GC data model, and safety invariants.
- **`CONTRIBUTING.md`**: Workspace crate structure, coding style, error handling, and testing guidelines.

# Code Quality & Refactoring Directives

- **Concise & Idiomatic Rust**: Write clean, modern, and idiomatic Rust. Prefer simplified expressions, standard combinators (e.g., `Option`/`Result` combinators), and clean control flow over overly verbose logic.
- **Signal-to-Noise Ratio in Comments**:
  - **Avoid Redundant Comments**: Do not write comments that merely restate what the code clearly expresses (e.g., `// create a new vector`, `// return the result`).
  - **Keep Value-Additive Comments**: Retain or add comments only when explaining complex algorithm choices, subtle invariants, safety guarantees (`// SAFETY:`), or architectural context.
- **Self-Documenting Code**: Prefer expressive function, variable, and type names over heavy comment explanations.

# Code Generation Directives

- Respect crate boundaries inside `crates/*` and adhere strictly to the unidirectional dependency flow defined in `CONTRIBUTING.md`.
- Never use `unwrap()`, `expect()`, or `panic!` in non-test library code.
- Write code comments, documentation, and commit messages in English.
