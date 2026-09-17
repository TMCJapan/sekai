# Overview & System Boundaries

## Purpose

A chunk-level deduplicated backup and rollback tool for Minecraft region
files (`.mca`), designed for frequent hot-recovery rather than long-term
archival, and a reusable Rust library for third-party server-management
software.

## Scope & Boundaries

- **In Scope**: Anvil sector parsing, chunk payload extraction, NBT
  decompression/normalization for diffing, content-addressed blob storage
  (CAS), MVCC metadata management over pluggable DB backends, atomic
  rollback, and GC.
- **Out of Scope**: External process lifecycle, server I/O synchronization
  (`save-off`, `save-all`, `save-on`), and cron/scheduling. These belong
  exclusively to caller software or administrators - never to library crates.

## Layer Architecture

Dependencies point inward. `core` never depends on `world`, `storage`, or
`app`; `util` is a dependency leaf (nothing in the workspace depends on
anything through it).

```text
sekai (bin, thin: clap parse + print + exit code)
  -> sekai-app (std + tokio: orchestration, parallelism, clocks, timing)
    -> sekai-core (no_std + alloc: use-case policy API over shared types)
      -> sekai-anvil (no_std + alloc: pure sector codec, decompress)
      -> sekai-nbt (no_std + alloc: pure parse/diff)
    -> sekai-world (std: filesystem owner - discover/fingerprint/scan/swap)
      -> sekai-anvil + sekai-core (types only)
    -> sekai-storage (std + tokio: async MetaStore/BlobStore traits,
                      file CAS, and feature-gated DB backends)
      -> sekai-core
sekai-util (no_std, zero-dependency: coordinates, hashes, history value
  types) <- anvil, nbt, core (world/storage/app reach them through
  `core` re-exports)
```

| Crate | `std` | `async` | Role |
|---|---|---|---|
| `sekai-util` | `no_std`, zero external dependencies | no | Shared value types: coordinates, hashes, region/history/snapshot/gc records, hex errors. Pure data plus total validation only. |
| `sekai-anvil` | `no_std` + `alloc` | no | Pure Anvil sector codec including decompression. `&[u8]` sector payload in, raw NBT bytes out; also builds `Vec<u8>` images. Compression framing is MCA spec knowledge, so it lives here. No `Path`, no `fs`. Names custom dimensions with util `Dimension`. |
| `sekai-nbt` | `no_std` + `alloc` | no | Pure `raw NBT bytes -> diff view`. Hand-rolled NBT parser (no `Serialize` by design; output uses SNBT), tag filtering, canonical digest returning util `DiffHash`. Never sees compression. |
| `sekai-core` | `no_std` + `alloc` | no (`async` only as trait bounds, see below) | Use-case policy API (`plan`/`assemble`/`commit`, `plan_rollback`, `gc plan`/`apply`) plus composition helpers (`hash_blob`, `resolve_custom_dimension`, storage ports). Composes `anvil`/`nbt` over util types and re-exports them, so downstream paths stay stable. |
| `sekai-storage` | `std` + tokio | yes (RPITIT) | Single crate: async `BlobStore`/`MetaStore` traits (owned by `core::port`, re-exported from the `api` module), file CAS (`cas` module, `blobs/ab/cdef...` retained by design), and feature-gated DB backends (`sqlite` module ships; `mysql`/`postgres` modules reserved as stubs). Depends on `core`. Backend modules mirror the future split so extraction stays mechanical. |
| `sekai-world` | `std` | yes (I/O) / sync pure helpers where trivial | Filesystem owner: world discovery, fingerprint observation, read-only scans, atomic swaps. |
| `sekai-app` | `std` + tokio | yes | Composition/orchestration library for third parties: parallel ingest, clocks, timing, progress callbacks. |
| `sekai` bin (`sekai-cli` package) | `std` + tokio | yes | Thin clap wrapper over `sekai-app`. No logic. Binary name stays `sekai`; package name stays `sekai-cli` so existing CI/artifact names keep working. |

### Why `core` must not depend on `world`

- `core` is `no_std`; `world` is inherently `std` (`Path`, `fs`, `mtime`).
  A `core -> world` edge would poison `core` with `std` and kill
  `thumbv7m`/`wasm` portability and third-party embedding.
- `world` already depends on `core` types, so the reverse edge would be a
  package cycle (forbidden by Cargo).
- Correct direction: `world` observes the filesystem and produces `core`
  input structs (`Observation`, `RegionFingerprint`); `app` feeds them into
  `core::plan_*`. This is dependency inversion: `core` owns the shapes,
  outer crates produce them.

### Shared value types (`sekai-util`)

Coordinates, hashes, and history records live in a zero-dependency leaf so
`anvil` and `nbt` can name the same types as `core` without a package
cycle (`core -> anvil/nbt -> core` is forbidden by Cargo). The boundary
rules:

- `util` holds pure data plus total validation only: no hashing backends,
  no codecs, no ports, no orchestration. It depends on nothing.
- `anvil`/`nbt` implement over util types (`Dimension` custom ids,
  `DiffHash` digests); `core` composes them (`hash_blob`,
  `resolve_custom_dimension`, storage ports, use cases).
- `core` re-exports every util type at its root, so downstream paths
  (`sekai_core::ChunkCoord`, ...) keep working; `world`/`storage`/`app`
  continue to import through `core`.
- Codec errors (`UnknownCompression`, `CustomCompression`, `ExternalBody`,
  decompression failures) are `anvil` errors: compression framing is MCA
  spec knowledge. `nbt` input is always already-decompressed raw NBT; it
  knows nothing of compression-type bytes or sector framing.

## Data Flow

```text
backup:
  world: Path -> Vec<u8> image
    -> anvil: image -> sector payloads -> decompress -> raw NBT bytes
    -> core::hash_blob(sector payload) -> BlobHash -> storage CAS put
    -> nbt (opt-in only): diff_hash(raw NBT bytes) -> diff cache column

rollback:
  core::plan_rollback -> groups of (coord, BlobHash)
    -> storage CAS fetch -> anvil builder -> Vec<u8> image
    -> world::atomic_swap(target, image)
```

`blob_hash` runs over the opaque sector payload (compression framing
included); `diff_hash` runs over the decompressed raw NBT bytes. The two
inputs diverge at the `anvil` decompression step, which is why decompression
belongs to `anvil`: it is the only crate that understands the sector
framing.

`nbt` is an opt-in path (`with_diff` flag); `anvil` never calls `nbt`
directly (no `anvil -> nbt` edge); `core`/`app` drives `anvil`'s
decompressed raw NBT bytes into `nbt` so the layering stays acyclic and
the hot path stays decode-free.

## World Layouts

Three server families share the `.mca` format but not the directory
layout. Pass a server root for Bukkit-family servers or a single world
folder for vanilla ones — the same path on every run.

- Vanilla: one folder (`region/`, `DIM-1/`, `DIM1/`, and since 26.1
  `dimensions/minecraft/<name>/`).
- Bukkit-family (Bukkit/Spigot/Paper/Purpur, pre-26.1 layout): split
  folders `<base>/`, `<base>_nether/DIM-1/`, `<base>_the_end/DIM1/`,
  where `base` is `level-name` (`world` by default). Paper 26.1+
  migrates to the vanilla layout.
- Plugin worlds (Multiverse et al.): arbitrary folders, detected by
  contents (`level.dat`, `DIM-1`/`DIM1` nesting, or kind directories
  holding region files).

Namespaces: the default trio and `minecraft/*` trees keep vanilla codes
(derivable, history-stable, migration-continuous); every other folder
hashes its root-relative path so distinct worlds never silently share
coordinates. Trio detection runs on container roots only, so a nested
folder can never steal the vanilla namespace. Missing files under
non-derivable codes fail loudly (`UnknownRegionPath`); rollback restores
those through their discovered folders, preferring same-dimension
siblings over flavor derivation when folders moved since the backup.

# Data & Hashing Model

## Two-Layer Hashing Architecture

- `blob_hash` (CAS key): hash over the exact raw byte payload as read from
  the sector (compression framing included). Immutable, persistent, restore
  path. Rollback reproduces captured bytes verbatim, rewinding volatile tags
  (e.g. `LastUpdate`) to capture time.
- `diff_hash` (volatile diff view): hash over uncompressed NBT with
  non-essential tags excluded (treated as absent), over a canonical encoding
  (compound keys sorted, big-endian scalars, UTF-8 strings) - never over
  re-serialized NBT bytes, so it is independent of on-disk order and codec.
  V1 rules exclude `LastUpdate` and `InhabitedTime`. Used only in-memory or as a cache column;
  never alters stored blobs.

## Chunk Codec Support

Region sectors frame payloads as compression-type byte + body. Supported
vanilla codecs are `1` (Gzip), `2` (Zlib), `3` (Uncompressed), and `4` (LZ4
since 24w04a, lz4-java `LZ4BlockOutputStream` framing — not standard LZ4).
Decompression lives in `sekai-anvil`: the framing byte is MCA spec
knowledge, so `anvil` exposes `decompress_into(payload, &mut out)` and
owns all codec errors — type `127` surfaces as `CustomCompression`,
`>= 128` (external `c.<x>.<z>.mcc` body) as `ExternalBody`, anything else
as `UnknownCompression`. `sekai-nbt` only ever receives already-decompressed
raw NBT bytes and parses them with a hand-rolled `no_std` parser (no
`Serialize` by design). Backup ingests raw sector payloads opaquely into CAS;
unsupported codecs surface only when the raw NBT view is derived for
diffing, never as ingest failures.

### Decompression plan (`anvil::decompress_into`)

- Type `3` (raw): copy body after the type byte.
- Type `2` (zlib): `miniz_oxide::inflate::decompress_to_vec_zlib_with_limit`
  (header + adler handled internally).
- Type `1` (gzip): parse the gzip header manually (magic `1F 8B`, method `8`,
  flags: `FTEXT/FHCRC/FEXTRA/FNAME/FCOMMENT`), run
  `miniz_oxide::inflate::decompress_to_vec_with_limit` over the raw deflate
  stream, then verify the footer (`crc32fast` over output + `ISIZE`).
- Type `4` (lz4-java stream): parse the framing (see lz4-java framing spec
  in `crates/anvil/src/lib.rs`), decode each body with the `lz4_flex` block
  API (`Raw` bodies are copied), verify the per-block XXH32 checksum, stop at
  the empty (`decompressed_len == 0`) block.
- Types `127` / `>= 128` / other: `CustomCompression` / `ExternalBody` /
  `UnknownCompression` — rejected before any allocation.
- Always prefer the `*_with_limit` variants: chunk payloads are untrusted
  input and unbounded allocation is a DoS vector.

## CAS Consistency

File CAS layout (`blobs/ab/cdef...`) is retained by design. Normalization
rule updates affect only change detection - zero data migration or blob
re-indexing.

# History, State, and GC Model

## MVCC & Interval Coverage

Unchanged: per-coordinate (`dim`, `kind`, `chunk_x`, `chunk_z`) timelines;
entries cover snapshot intervals `[S_i, S_{i+1})`; MVCC timeline, not a
commit DAG.

## Delta History Storage

Snapshots store only fresh rows (ingested chunks plus tombstones).
Unchanged chunks have no row at newer snapshots; reads resolve effective
state through fallback - `lookup_chunk` returns the nearest row at or
before the snapshot, `visit_snapshot_chunks` synthesizes one effective row
per known coordinate. Carried regions advance only derived state
(`region_state`), and their effective chunk count is aggregated for the
report. Every backup therefore grows metadata with changes, not with
universe size - an unchanged backup writes one snapshot row plus state
bumps instead of a full row copy.

## Tombstones

Missing chunks are explicit `NULL`-blob rows. Rollback onto tombstones
removes sectors (fully tombstoned regions delete the file rather than
leaving a header-only shell).

## Scoped Operations

Backup, rollback, and diff accept a scope: whole world (default) or
per-dimension areas — whole dimension (`--in DIM`), chunk rectangle
(`--in DIM:x0,z0..x1,z1`), explicit chunks (`--in DIM:x,z`), or region
files (`--region DIM:RX,RZ`, rectangle sugar). Region kinds (`--kind`,
repeatable; empty means all) apply uniformly across every area. The
scope is a filter, not a partition:

- Scoped backup ingests, carries, and tombstones only inside the scope.
  Out-of-scope coordinates record no rows and no tombstones, resolving
  through fallback from earlier snapshots; stored state rows outside the
  scope are never marked removed. A scoped backup therefore reads as the
  truth for its scope only - a file deleted inside the scope tombstones
  its chunks, while anything outside is untouched.
- Scoped rollback rebuilds and deletes only inside the scope; files
  outside are never written, deleted, or otherwise touched.
- Multi-chunk diff resolves coordinates from the snapshots (or world
  state) filtered by the scope and omits chunks without differences.

## Single Source of Truth vs Derived State

- **Source of truth**: metadata history (`snapshots`, `chunk_history`) in the
  selected DB backend + CAS blobs.
- **Derived state**: `region_state` fingerprints. Wiping them costs at most
  one full ingest, never wrong data.

## Garbage Collection (GC)

- **Reference scope**: global (all snapshots/dimensions/chunks), matching
  global CAS dedup.
- **Two-phase safety**: `gc plan` (read-only candidates) then `gc apply`
  (re-verified unlink). No metadata writes in the orphan-only scope.
- **Snapshot pruning** (`prune`): oldest-first fold into the next
  retained snapshot, then delete. Pruning dereferences blobs only;
  `gc` reclaims them afterwards.

# Storage Backends

## Trait/Impl Split (single crate, feature-gated modules)

- `sekai-storage` is a single crate. The async `BlobStore`/`MetaStore`
  traits (RPITIT, `Send` bounds for tokio) are owned by `core::port` and
  re-exported from storage's `api` module, which additionally owns
  `BackendKind` and URL-based selection (`sqlite://...`, `mysql://...`,
  `postgres://...`).
- DB backends are modules behind cargo features: `sqlite` (sqlx, ships in
  this project), `mysql` / `postgres` (reserved stubs implementing the same
  traits). No generic `Any`-driver abstraction.
- Module boundaries (`api` / `cas` / `sqlite` / `mysql` / `postgres`) mirror
  the future crate split 1:1, so extracting a backend into its own crate
  later is mechanical. Crate splitting is deferred until a second backend is
  actually implemented (YAGNI).
- This project currently ships SQLite only; mysql/postgres arrive as modules
  without touching `core`/`app`.

## Backend Selection

Feature-gated plus runtime-selected: `sekai-storage` exposes cargo features
per backend (`default = ["backend-sqlite"]` for a lean binary, re-exported
through `app`/`sekai-cli` features); at runtime
the connection URL picks the compiled-in backend, and requesting an
uncompiled backend is a loud `UnsupportedBackend` error — never silent
reinterpretation.

## Schema

Shared across backends (`snapshots`, `chunk_history`, `region_state`;
`SCHEMA_VERSION` gate; pre-release policy = recreate, don't migrate).
The `cas` module stays independent of the DB backend modules so future
object-store CAS swaps don't touch metadata code.

## Async Policy

- `async` lives in `storage`, `world`, `app`, and the bin - never in
  `anvil`/`nbt`/`core` logic. `core` uses `async fn` in traits (RPITIT,
  no `async-trait` macro, no boxing, static dispatch preserved) so
  implementations can be `tokio`-backed while policy stays runtime-free.
- Pure orchestration steps (`assemble`, image building, canonical digest)
  stay synchronous.

# Rollback & I/O Invariants

- **Atomic writes & cross-device safety**: `.mca` updates go to a temp file
  **in the same directory as the target** + `fsync` + `rename`. In-place
  mutation is prohibited. `world::atomic_swap` is the single owner of this
  path; `anvil` only builds the `Vec<u8>` image.
- **Sector alignment**: writes always land on 4 KiB boundaries with header
  offset-table integrity. Reads tolerate trailing partial sectors (e.g. torn
  tails from interrupted saves, which vanilla also opens): every referenced
  sector run is validated against exact byte bounds instead, and genuinely
  corrupt runs still fail loudly with the file path attached. `world` never
  patches bytes.
- **Crash consistency order**:
  - **Write path**: CAS blobs flushed + synced *before* DB metadata commit.
  - **GC delete path**: DB metadata commit *before* blob unlink.
  - **Prune path**: each snapshot's fold (row restamp/drop, state
    repoint, snapshot delete) commits atomically; blobs unlink only in
    a later `gc`.
- **No server orchestration in libraries**: `save-off`/`save-all`,
  scheduling, and process control belong to callers, never to `core`,
  `anvil`, `nbt`, `storage`, `world`, or `app` internals.
- **Rebuildable derived state** and **two-phase destructive operations**
  (plan before apply) hold for every change.
