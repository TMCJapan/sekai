# Project Context

This workspace contains a high-frequency, chunk-level deduplicated backup and rollback tool for Minecraft region files (`.mca`), written in Rust.

# Reference Documentation

Prior to generating, refactoring, or reviewing code, strictly follow the specifications in:
- **`ARCHITECTURE.md`**: System boundaries, core abstraction policy, 2-layer hashing, MVCC/GC data model, and safety invariants.
- **`CONTRIBUTING.md`**: Workspace crate structure, coding style, error handling, and testing guidelines.

# Non-Negotiable Safety & Design Invariants

When generating or modifying code, you MUST comply with the following invariants:

1. **`core` Crate Purity**:
   - `crates/core` MUST remain `no_std` with **zero external dependencies**. All I/O, SQLite operations, and Gzip/NBT parsing must be abstracted via traits in `core` and implemented in outer crates (`mca`, `storage`, `nbt`).

2. **Atomic File I/O Only**:
   - Never overwrite `.mca` files in place. Always write changes to a temporary file **in the same directory as the target `.mca` file** (to ensure atomic `fs::rename` across mounts) and finalize via swap.

3. **Byte-Perfect Rollback**:
   - Store raw, compressed chunk payloads directly in CAS (Content-Addressable Storage).
   - Normalization must be applied ONLY for calculating non-persistent diff views, never mutating the raw storage payloads.

4. **Clear Boundary Separation**:
   - Do NOT embed external process orchestration (e.g., stopping servers, running `save-off`/`save-all`/`save-on`) inside core libraries. Keep system integration strictly on the CLI/caller layer.

5. **Rebuildable Derived State**:
   - Treat lookup caches and state tables (e.g., `chunk_state`) as purely derived data. Ensure they can always be fully reconstructed from immutable snapshot/blob history.

6. **Safe Mutating Operations**:
   - Any destructive or mutating operation (e.g., GC purging) MUST separate the planning phase (`dry-run`) from the execution phase (`apply`).

# Code Generation Directives

- Respect crate boundaries inside `crates/*` and adhere strictly to the unidirectional dependency flow defined in `CONTRIBUTING.md`.
- Never use `unwrap()`, `expect()`, or `panic!` in non-test library code.
- Write code comments, documentation, and commit messages in English.


