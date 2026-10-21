# Overview & System Boundaries

## Purpose

A chunk-level deduplicated backup and rollback tool for Minecraft region
files (`.mca`), designed for frequent hot-recovery rather than long-term
archival, and a reusable Rust library for third-party server-management
software.

## Scope & Boundaries

- **In Scope**: Anvil sector parsing, chunk payload extraction, NBT
  decompression/normalization for diffing, content-addressed blob storage
  (CAS), history-graph metadata management over pluggable DB backends,
  atomic rollback, and GC.
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
    -> sekai-world (std: filesystem owner - discover/fingerprint/swap)
      -> sekai-anvil + sekai-core (types only)
    -> sekai-storage (std + tokio: store traits, file CAS, and locks)
      -> sekai-core
    -> sekai-storage-sqlite (std + tokio: sqlx SQLite implementation)
      -> sekai-storage
sekai-util (no_std, zero-dependency: coordinates, hashes, history value
  types) <- anvil, nbt, core (world/storage/app reach them through
  `core` re-exports)
```

| Crate | `std` | `async` | Role |
|---|---|---|---|
| `sekai-util` | `no_std`, zero external dependencies | no | Shared value types: coordinates, hashes, region/history/snapshot/gc records, hex errors. Pure data plus total validation only. |
| `sekai-anvil` | `no_std` + `alloc` | no | Pure Anvil sector codec including decompression. `&[u8]` sector payload in, raw NBT bytes out; also builds `Vec<u8>` images. Compression framing is MCA spec knowledge, so it lives here. No `Path`, no `fs`. Names custom dimensions with util `Dimension`. |
| `sekai-nbt` | `no_std` + `alloc` | no | Pure `raw NBT bytes -> diff view`. Hand-rolled NBT parser (no `Serialize` by design; output uses SNBT), tag filtering, canonical digest returning util `DiffHash`. Never sees compression. |
| `sekai-core` | `no_std` + `alloc` | no (`async` only as trait bounds, see below) | Use-case policy API (`plan`/`assemble`/`commit`, `plan_rollback`, `gc plan`/`apply`) plus composition helpers (`hash_blob`, storage ports). Composes `anvil`/`nbt` over util types and re-exports them, so downstream paths stay stable. |
| `sekai-storage` | `std` + tokio | yes (RPITIT) | Backend-neutral boundaries: async `BlobStore`/`MetaStore` traits (owned by `core::port`, re-exported from the `api` module), high-level `SekaiStore`/`SekaiStoreOpener`/`BlockingBlobStore` traits, file CAS (`cas` module, `blobs/ab/cdef...` retained by design), store locks, and URL-based backend selection. Depends on `core`. |
| `sekai-storage-sqlite` | `std` + tokio | yes | Concrete SQLite implementation over sqlx: `SqliteMeta` plus the SQLite-backed `SqliteStore`, schema/version gate, and opener. Implements the traits from `sekai-storage` (and `core::port`). Other backends become sibling crates without touching `core`/`app`. |
| `sekai-world` | `std` | yes (I/O) / sync pure helpers where trivial | World access boundary: the `WorldTree` trait, the `RegionFile` handle, and the filesystem tree (`HostWorldTree`). |
| `sekai-app` | `std` + tokio | yes | Composition/orchestration library for third parties: parallel ingest, clocks, timing, progress callbacks. |
| `sekai` bin (`sekai-cli` package) | `std` + tokio | yes | Thin clap wrapper over `sekai-app`. No logic. Binary name stays `sekai`; package name stays `sekai-cli` so existing CI/artifact names keep working. |

### Why `core` must not depend on `world`

- `core` is `no_std`; `world` is inherently `std` for its host
  implementation (`Path`, `fs`, `mtime`).
  A `core -> world` edge would poison `core` with `std` and kill
  `thumbv7m`/`wasm` portability and third-party embedding.
- `world` already depends on `core` types, so the reverse edge would be a
  package cycle (forbidden by Cargo).
- Correct direction: `world` observes the filesystem and produces `core`
  input structs (`Observation`, `RegionFingerprint`); `app` feeds them into
  `core::plan_*`. This is dependency inversion: `core` owns the shapes,
  outer crates produce them.

### The world access boundary

`world` owns the boundary between the operations and the files they
touch. A `WorldTree` names the region files it holds, derives the ones
that are missing, and hands out `RegionFile` handles: a handle reads,
fingerprints, writes, and removes one region file, and nothing else. The
operations never see a path, so the flows do not depend on the tree
behind them. See
[ADR-0015](docs/adr/0015-world-region-handles.md).

### Shared value types (`sekai-util`)

Coordinates, hashes, and history records live in a zero-dependency leaf so
`anvil` and `nbt` can name the same types as `core` without a package
cycle (`core -> anvil/nbt -> core` is forbidden by Cargo). The boundary
rules:

- `util` holds pure data plus total validation only: no hashing backends,
  no codecs, no ports, no orchestration. It depends on nothing.
- `anvil`/`nbt` implement over util types (`Dimension` custom ids,
  `DiffHash` digests); `core` composes them (`hash_blob`, storage ports,
  use cases).
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
  world: region handle -> Vec<u8> image
    -> anvil: image -> sector payloads -> decompress -> raw NBT bytes
    -> core::hash_blob(sector payload) -> BlobHash -> storage CAS put
    -> nbt (opt-in only): diff_hash(raw NBT bytes) -> diff cache column

rollback:
  core::plan_rollback -> groups of (coord, BlobHash)
    -> storage CAS fetch -> anvil builder -> Vec<u8> image
    -> world: region handle <- atomic swap(target, image)
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
  where `base` is `level-name` (`world` by default); each datapack
  dimension `<namespace>:<path>` gets its own sibling world folder
  `<base>_<namespace>_<path>/dimensions/<namespace>/<path>/`.
  Paper 26.1+ migrates to the vanilla layout.
- Plugin worlds (Multiverse et al.): arbitrary folders, detected by
  contents (`level.dat`, `DIM-1`/`DIM1` nesting, or kind directories
  holding region files).

Namespaces: every observed dimension is keyed by a hash of its canonical
`<namespace>:<name>` id; the default trio hashes to the `Dimension`
constants (`OVERWORLD`/`NETHER`/`END`), so ids are history-stable across
layout changes (legacy, 26.1, Bukkit) and distinct dimensions never
silently share coordinates. Trio detection runs on container roots only,
so a nested folder can never steal the vanilla namespace. Opening a tree
classifies the root and resolves one directory per observed dimension
once; operations then use those resolved handles and never re-inspect the
layout. Region handles are derivable only for the ids a tree resolved;
anything else fails loudly (`UnknownRegionPath`) instead of writing
somewhere wrong, and rollback restores those files through their
discovered folders, preferring same-dimension siblings over derived
handles when folders moved since the backup.

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
  `miniz_oxide::inflate::core::decompress` over the raw deflate
  stream, require it to consume the body exactly so the 8-byte trailer
  starts where the stream ends, then verify the footer (`crc32fast` over
  output + `ISIZE`).
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

## The Snapshot Graph

Snapshots form a history graph, not a timeline: every snapshot names the
snapshots it continues in `parents` (empty for the root of a history, one
entry for an ordinary snapshot, several for a merge). The graph - not the
identifier order - decides what a snapshot inherits, so a snapshot never
sees rows introduced by a history it does not descend from.

Parent links are written with the snapshot row, in the same transaction,
and read back through the `MetaStore` port (`snapshot_parents`) or walked
to the root by the core use cases (`usecase::snapshot::ancestors` /
`history`).

Ref objects (a movable `HEAD`, named branches) do not exist yet. Until
they do, a new snapshot continues the snapshot it was planned against: the
latest snapshot of the store, which is also the base its delta was
computed from. Merges are representable in the model but nothing creates
them yet, so a history is a chain in practice.

Retiring a snapshot (`prune`) keeps the graph connected: the children of
the retired snapshot continue the history of its own parents, so the
surviving snapshots resolve to the same effective state as before.

## Delta History Storage

Snapshots store only fresh rows (ingested chunks plus tombstones).
Unchanged chunks have no row at newer snapshots; reads resolve effective
state through the graph - `lookup_chunk` returns the row introduced at
the snapshot or at one of its ancestors (the nearest one wins),
`visit_snapshot_chunks` synthesizes one effective row per known
coordinate. Carried regions advance only derived state (`region_state`),
and their effective chunk count is aggregated for the report. Every backup
therefore grows metadata with changes, not with universe size - an
unchanged backup writes one snapshot row plus state bumps instead of a
full row copy.

## Tombstones

Missing chunks are explicit `NULL`-blob rows, written once at the snapshot
where they vanish: a coordinate already tombstoned records nothing further,
because its absence already resolves through fallback. Change detection
therefore compares the world against the effective *present* coordinate set
of the latest snapshot, so a repeated backup of an unchanged world writes no
chunk rows at all. Rollback onto tombstones removes sectors (fully
tombstoned regions delete the file rather than leaving a header-only shell).

## Scoped Operations

Backup, rollback, and diff accept a scope: whole world (default) or
per-dimension areas — whole dimension (`--in DIM`), chunk rectangle
(`--in DIM:x0,z0..x1,z1`), explicit chunks (`--in DIM:x,z`), or region
files (`--region DIM:RX,RZ`, rectangle sugar). Region kinds (`--kind`,
repeatable; empty means all) apply uniformly across every area, and on
their own they narrow the whole world to those families (`Scope::Kinds`).
The scope is a filter, not a partition:

- Scoped backup ingests, carries, and tombstones only inside the scope.
  Out-of-scope coordinates record no rows and no tombstones, resolving
  through fallback from earlier snapshots; stored state rows outside the
  scope are never marked removed. A scoped backup therefore reads as the
  truth for its scope only - a file deleted inside the scope tombstones
  its chunks, while anything outside is untouched. A file the scope covers
  only partly records no fingerprint and drops its stored one, so the next
  backup that reaches it reads it again instead of carrying unread chunks.
- Scoped rollback rebuilds and deletes only inside the scope; files
  outside are never written, deleted, or otherwise touched.
- Multi-chunk diff resolves coordinates from the snapshots (or world
  state) filtered by the scope and omits chunks without differences.

## Single Source of Truth vs Derived State

- **Source of truth**: metadata history (`snapshots`, `snapshot_parents`,
  `chunk_history`) in the selected DB backend + CAS blobs.
- **Derived state**: `region_state` fingerprints. Wiping them costs at most
  one full ingest, never wrong data.

## Garbage Collection (GC)

- **Reference scope**: global (all snapshots/dimensions/chunks), matching
  global CAS dedup.
- **Two-phase safety**: `gc plan` (read-only candidates) then `gc apply`
  (re-verified unlink). No metadata writes in the orphan-only scope.
- **Exclusivity**: a run that writes blobs holds `backup.inflight` in the
  store root for its duration; `gc` refuses at plan and apply time while it
  is held, because a backup's blobs are in the CAS before the rows that
  reference them. Readers (`rollback`, `export`, `diff`) never take it. See
  [ADR-0011](docs/adr/0011-backup-run-marker.md).
- **Snapshot pruning** (`prune`): oldest-first fold into the next
  retained snapshot, then delete. Pruning dereferences blobs only;
  `gc` reclaims them afterwards.

# Storage Backends

## Trait/Impl Split (backend crates)

- `sekai-storage` owns the backend-neutral boundaries. The async
  `BlobStore`/`MetaStore` traits (RPITIT, `Send` bounds for tokio) still
  come from `core::port` and are re-exported from storage's `api` module.
  Storage adds the high-level `SekaiStore` (metadata + blobs + the store
  lock), `SekaiStoreOpener` (URL selection), and `BlockingBlobStore` (the
  synchronous blob operations used by backup ingest workers).
- `sekai-storage-sqlite` is the concrete sqlx backend. It owns `SqliteMeta`,
  `SqliteStore`, the schema, and the version gate, and implements the
  storage/core traits. Removing sqlx from `sekai-storage` keeps the
  abstraction crate free of a native database dependency.
- New backends (mysql, postgres, embedded, in-memory, wasm) become sibling
  crates implementing the same traits. `app` is generic over
  `SekaiInstance<T: SekaiStore>`, so adding a backend does not touch use-case
  policy. No generic `Any`-driver abstraction.
- SQLite is the only backend shipped. `sekai-storage` holds no backend
  modules at all: it owns the traits, the file CAS, and the locks, and stays
  free of a native database dependency. A mysql or postgres backend arrives
  as a sibling crate, never as a stub inside `sekai-storage`.

## Backend Selection

Runtime-selected, with compile-time gating on the backend crate: `app`
exposes a `backend-sqlite` feature (default-on) that pulls in
`sekai-storage-sqlite`; `sekai-cli` inherits it through `sekai-app` and
needs no features of its own. At runtime the connection URL picks the
backend, and requesting a backend this build does not carry is a loud
`UnsupportedBackend` error — never silent reinterpretation.
`sekai-storage` keeps `BackendKind` and URL parsing; each
backend crate implements `SekaiStoreOpener` for its store type, so the
backend owns the grammar of the URLs that name it.

A build without `backend-sqlite` has no store-opening entry point at all:
`sekai-storage` ships no stand-in store, so a build that carries no backend
cannot open one and says so at compile time.

## Store Exclusion

`SekaiStore::lock` hands out the store-wide exclusion lock that ADR-0014
assigns per operation. The lock lives in the store root, so it excludes
operations against the same store on the same host; a remote backend needs
its own mechanism (ADR-0014). The guard is `Send + Sync` because an
operation shares it into the blocking tasks that touch files, so a
cancelled future cannot release the store while those tasks still run.

## Schema

Owned by the backend crate (`crates/storage-sqlite/schema/sqlite.sql` for
SQLite; `snapshots`, `snapshot_parents`, `chunk_history`, `region_state`;
`SCHEMA_VERSION` gate; pre-release policy = recreate, don't migrate).
The `cas` module stays independent of DB backend crates so future
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
  path, reached through the region handle; `anvil` only builds the `Vec<u8>`
  image.
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
  (plan before apply) hold for every change. A fingerprint must therefore be
  a *content* signal: hashing only part of a file would let a changed file
  look unchanged, which is wrong data rather than a cache miss
  ([ADR-0012](docs/adr/0012-fingerprint-covers-file-content.md)).
