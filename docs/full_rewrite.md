# Full Rewrite Plan

> **Note**: This document is a temporary execution plan for the rewrite. **This file will be deleted once the rewrite is complete.**

## Background

The pre-rewrite workspace (`core` zero-dependency port hub, `mca` mixing pure
sector math with `std::fs` walks, `nbt` with no callers, `cli` doubling as
the composition library) grew organically and no longer matches its own
`ARCHITECTURE.md`. The backup hot path never populated the `diff` cache, so
the `nbt` crate was dead code and the documented two-layer hashing flow was
broken in practice.

Goal of this rewrite: a clean, layered architecture maximizing library
reuse for third-party Rust server-management software, with `no_std` pure
cores, pluggable async storage backends, and a thin binary.

`ARCHITECTURE.md` and `CONTRIBUTING.md` have already been rewritten to the
target shape. This document is the execution plan from the old tree to that
shape: **spec-freeze -> delete -> rebuild from zero**.

## Locked Decisions

1. **`sekai-world` (new).** Filesystem owner: world discovery, fingerprint
   observation, read-only scans, `open_image`, `atomic_swap`. `mca`'s fs
   half moves here; `core` never touches `fs`.
2. **`sekai-mca` -> `sekai-anvil`, `no_std` + `alloc`.** Pure sector codec
   **plus decompression** (`&[u8]` sector payload in, raw NBT bytes out;
   also builds `Vec<u8>` images). Compression framing is MCA spec, so it
   lives here. The crate rename already landed; the `no_std` split is part
   of this rewrite. Deps: `miniz_oxide` + `crc32fast` (gzip/zlib),
    `lz4_flex` block API + `twox-hash` (lz4-java stream); `lz4-java-wrc`
    and `flate2` are banned.
3. **`nbt` takes raw NBT bytes, opt-in.** Data flow is `anvil` output
   (decompressed) -> `nbt` input, driven by `core`/`app` - never
   `anvil -> nbt` directly (keeps the DAG acyclic). The parser is
   hand-rolled over the serde data model (`fastnbt` removed). `diff_hash`
   stays off the hot path (`with_diff` flag); `chunk_history.diff`
   remains an opt-in cache.
4. **`core` is an API layer depending on `anvil` + `nbt`.** It owns domain
   types + policy (`plan`/`assemble`/`commit`, `plan_rollback`,
   `gc plan`/`apply`). `core` must **not** depend on `world`/`storage`/`app`
   (would poison `no_std` and create a package cycle). Pure boundaries speak
   plain data (`&[u8]`, `i32`, `[u8; 32]`); `core::domain` converts.
5. **Storage: single crate, SQLite only now, modules ready for later.**
   `sekai-storage` owns async `BlobStore`/`MetaStore` traits (`api`
   module) + `BackendKind`/URL selection, the file CAS (`cas` module,
   `blobs/ab/cdef...` retained), and feature-gated backend modules
   (`sqlite` via sqlx ships; `mysql`/`postgres` are reserved stubs).
   Module boundaries mirror the future crate split 1:1 so extraction
   later is mechanical; the split itself is deferred until a second
   backend is actually implemented.
6. **Async from the start via RPITIT.** `async fn` in traits, `Send`-bounded
   for tokio, static dispatch. No `async-trait` macro, no boxing in `core`.
   `async`/tokio lives in `storage`, `world`, `app`, bin only.
7. **`sekai-app` composition library + thin `sekai` binary.** `app` owns
   parallel ingest (tokio), clocks, timing, progress callbacks - the
   third-party entry point. The binary only parses args and prints output.
8. **CAS stays on files.** No DB-BLOB or object-store CAS in this rewrite.

## Phase 1 - Skeleton

- [x] Recreate workspace members with final package names (`sekai-anvil`,
      `sekai-nbt`, `sekai-core`, `sekai-storage`, `sekai-world`,
      `sekai-app`, `sekai-cli`; `sekai-cli` keeps binary name `sekai`).
- [x] Workspace `Cargo.toml`: resolver 3, edition 2024, shared lints,
      storage backend features
      (`default = ["backend-sqlite"]`, empty features until Phase 5 wires
      optional deps). Strip pre-rewrite deps (`rusqlite`, `fastnbt`,
      `flate2`, `lz4-java-wrc`); each phase adds only what it needs.
- [x] `rust-toolchain.toml` keeps `thumbv7m-none-eabi` +
      `wasm32-unknown-unknown` targets.
- [x] Exit: `cargo metadata` resolves, `cargo fmt --check` passes on empty
      crates.

## Phase 2 - `sekai-nbt` (pure, first: unblocks `core`)

Hand-rolled parser (reference the old `nbt` digest rules only, no
`std`/fastnbt copy-paste):

- [x] `#![no_std] + extern crate alloc`; hand-written `NbtError`
      (`Display`, no `thiserror`/`std::io::Error`).
- [x] Hand-rolled NBT parser over `&[u8]` raw NBT bytes (already
      decompressed by `anvil`) into an owned `Value` tree; serde data model
      only (`serde` with `default-features = false, features = ["alloc",
      "derive"]`). `fastnbt` is removed.
- [x] `diff_hash(&[u8], &[&str]) -> Result<[u8;32], NbtError>`,
      `diff_hash_v1` (ignores `["LastUpdate", "InhabitedTime"]`).
- [x] Canonical digest rules preserved (sorted keys, BE scalars, UTF-8).
- [x] Verify: `cargo test -p sekai-nbt`; clippy on both
      `thumbv7m-none-eabi` and `wasm32-unknown-unknown` with `-D warnings`.

## Phase 3 - `sekai-anvil` (pure/fs split)

Pure stays in `anvil`; fs moves to `world` (Phase 6):

- [ ] Keep pure: sector constants, `parse_region_name`, `RegionLoc`,
      `check_image_len`, `sectors_for`, `base_coords`,
      `from_bytes` + `visit_chunks(F: FnMut(RawChunk)->bool)`,
      `stage_chunk/stage_remove` + `image() -> Vec<u8>`,
      `header_hash(&[u8]) -> [u8;32]`, `custom_dim_id(&str)`.
- [ ] New: `compression_of(u8)` + `decompress_into(&[u8], &mut Vec<u8>)`
      (sector payload -> raw NBT bytes). Gzip/zlib via `miniz_oxide` +
      `crc32fast`; lz4-java stream via hand-rolled framing + `lz4_flex`
      block API + `twox-hash` checksums. All codec errors (`UnknownCompression`, `CustomCompression`,
      `ExternalBody`, decompression failures) live here.
- [ ] Decompression output is DoS-capped (`*_with_limit` variants).
- [ ] Drop from `anvil`: `open`, `swap`, `discover`, fd-based
      `fingerprint_file`, `scan_world`, `McaError::Io`.
- [ ] Pure error enum only (no `PathBuf`/`io::Error`).
- [ ] Boundary types are plain (`i32`, `&[u8]`, `[u8;32]`); `core` wraps.
- [ ] Verify: unit tests for damaged images/overflow/padding, all four
      codecs round-trip, corrupt-codec rejection + both cross-target
      clippy runs.

## Phase 4 - `sekai-core` (API layer)

- [ ] `domain` (coords, hashes, region, history, snapshot, gc) rewritten
      from scratch against the frozen API list.
- [ ] `hash_blob(&[u8]) -> BlobHash`; `nbt` bytes -> `DiffHash` wrapping.
- [ ] Policy: `plan_backup` / `assemble` / `commit` (sync pure),
      `plan_rollback`, `gc_plan` / `gc_apply`; DB-touching seams are
      `async fn` (RPITIT, `Send`-bounded) over generic backends.
- [ ] Delete old port traits (`RegionReader/Writer`, `Normalizer`,
      `BlobHasher/DiffHasher`); keep only async `BlobStore`/`MetaStore`
      usage via `storage::api` (same crate, no cycles: `storage -> core`).
- [ ] `diff` path is opt-in (`with_diff`); default `stage_present` records
      `diff: None`.
- [ ] Verify: `cargo test -p sekai-core` with in-memory fake backends
      (no `fs`, no SQLite).

## Phase 5 - `storage` (api + cas + sqlite module)

- [ ] `api` module: async traits, `BackendKind`, URL parsing
      (`sqlite://...`), `UnsupportedBackend` error, shared conformance test
      suite for future backend modules.
- [ ] `cas` module: file layout, same-dir temp + file fsync + batched
      `sync_dirs` barrier, `fetch_into`/`remove`/`visit_blobs`.
- [ ] `sqlite` module (sqlx, `backend-sqlite` feature): shared schema
      (`snapshots`, `chunk_history`, `region_state`), `SCHEMA_VERSION` gate,
      single-transaction `apply_snapshot_incremental` with the
      `INSERT ... SELECT` carry optimization, WAL + `synchronous=FULL`.
- [ ] `mysql` / `postgres` modules: stubs behind `backend-mysql` /
      `backend-postgres` features implementing the trait with
      `unimplemented!`/compile errors until scheduled.
- [ ] Decide `Store` shape (generic `Store<M: MetaStore>` vs cfg-gated
      backend enum); deferred to this phase, not Phase 1.
- [ ] Verify: version-gate test, carry test, derived-state-wipe test,
      torn-backup -> orphan-GC crash test, plus the backend feature matrix
      (`--no-default-features --features backend-sqlite`).

## Phase 6 - `sekai-world` (std)

- [ ] `discover`, `derive_path`, `detect_flavor`, `fingerprint_file`
      (fd `size/mtime` + `anvil::header_hash`), `scan_world`,
      `open_image`, `atomic_swap` (same-dir temp + fsync + rename + unix
      dir fsync, cleanup on failure).
- [ ] Verify: discovery fixture tests (legacy/Bukkit/new layouts),
      fingerprint match/mismatch tests, swap atomicity tests.

## Phase 7 - `sekai-app` + `sekai` bin

- [ ] `app::backup(world, store_url, BackupOptions{concurrency,with_diff}, progress)`
      and `app::rollback`, `app::list_snapshots`; tokio parallelism
      (`spawn_blocking` for ingest), wall clocks, `BackupTimings`.
- [ ] Crash order enforced at call sites: `cas.sync()` before DB commit;
      missing CAS blob aborts loudly (no partial worlds).
- [ ] `strict` rollback semantics preserved (post-snapshot files removed,
      all-tombstone regions deleted, not shelled).
- [ ] `sekai` bin: clap only (`backup --timing/--timing-json`, `rollback`,
      `list`, `debug scan` human/JSON). No logic.
- [ ] Verify: `tempdir` + file-CAS + SQLite round-trip
      (backup -> list -> rollback), timing smoke tests.

## Phase 8 - Docs, CI, Release

- [ ] Update `README*.md` migration note (old port traits gone, new
      `app` entry points, backend URL format).
- [ ] CI: fmt, workspace clippy, pure-crate cross-target clippy,
      `cargo test --workspace`,
      `cargo test -p sekai-storage --no-default-features --features backend-sqlite`,
      `cargo deny`, `cargo machete`, release build.
- [ ] Re-create `docs/` operational notes if needed.
- [ ] Delete this rewrite plan file (`docs/full_rewrite.md`).

## Risks

- **Audit drift.** Re-verify pinned dependency versions because a minor bump
  can flip a `no_std` cfg. Fallback if a dep regresses: vendor the minimal
  framing code into `anvil` (gzip header and lz4-java framing are both small,
  fully spec'd in `ARCHITECTURE.md` and `crates/anvil/src/lib.rs`) - but
  decide before Phase 2/3, not during.
- **Crash-order regressions** during the `image`/`swap` and
  `cas.sync`/`commit` splits. Covered by mandatory crash tests in
  Phases 5-7.
- **API sprawl.** Enforce the stable API list; everything else stays
  `pub(crate)` until a third-party need is proven.

## Exit Criteria

- `cargo fmt --all -- --check`, workspace clippy, pure cross-target
  clippy, `cargo test --workspace`, `cargo deny check`,
  `cargo build -p sekai-cli --release` all green.
- Backup -> list -> rollback round-trip green on Linux/macOS/Windows
  (`x86_64` + `aarch64`).
- No references to `sekai-mca`, old `cli` library root, sync `rusqlite`
  paths, `fastnbt`, `lz4-java-wrc`, `flate2`-in-`anvil`, or
  `RegionReader`/`Normalizer` traits remain.
