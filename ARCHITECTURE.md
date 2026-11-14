# Overview & System Boundaries

## Purpose

A chunk-level deduplicated version control tool for Minecraft region
files (`.mca`), designed for frequent hot-recovery rather than long-term
archival, and a reusable Rust library for third-party server-management
software.

## Scope & Boundaries

- **In Scope**: Anvil sector parsing, chunk payload extraction, NBT
  decompression/normalization for diffing, content-addressed blob storage
  (CAS), history-graph metadata management over pluggable DB backends,
  and atomic checkout.
- **Out of Scope**: External process lifecycle, server I/O synchronization
  (`save-off`, `save-all`, `save-on`), and cron/scheduling. These belong
  exclusively to caller software or administrators - never to library crates.

## Layer Architecture

Dependencies point inward. `core` never depends on `anvil`, `world`,
`storage`, or `app`; `util` is a dependency leaf (nothing in the workspace
depends on anything through it).

```text
sekai (bin, thin: clap parse + print + exit code)
  -> sekai-app (std + tokio: orchestration, parallelism, clocks, timing)
    -> sekai-core (no_std + alloc: use-case policy API over shared types)
    -> sekai-anvil (no_std + alloc: pure sector codec, decompress, NBT normalization)
    -> sekai-world (no_std + alloc: world access boundary - WorldTree trait, RegionFile handle)
      -> sekai-anvil + sekai-core (types only)
    -> sekai-world-local (std: host filesystem tree - discover/observe/swap, the world lock)
      -> sekai-world + sekai-anvil + sekai-core (types only)
    -> sekai-storage (std + tokio: store traits and locks)
      -> sekai-core
    -> sekai-storage-local (std + tokio: SQLite metadata + sled blobs)
      -> sekai-storage
sekai-util (no_std, no tokio dependency: coordinates, chunk hashes,
  history value types) <- anvil, core (world/storage/app reach them
  through `core` re-exports)
```

| Crate                             | `std`                                 | `async`                                      | Role                                                                                                                                                                                                                                                                                                                                                                                                 |
| --------------------------------- | ------------------------------------- | -------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `sekai-util`                      | `no_std`, no `std`/tokio dependencies | no                                           | Shared value types: coordinates, the chunk digest (`ChunkHash`, `hash_bytes`, `BytesHasher`), region/history/commit records, hex errors. Pure data plus total validation only.                                                                                                                                                                                                                    |
| `sekai-anvil`                     | `no_std` + `alloc`                    | no                                           | Pure Anvil sector codec including decompression and chunk NBT normalization. A stored sector payload becomes a `Chunk` whose `normalized_hash()` is the only public NBT-derived digest; the parser and normalized views stay internal. Owns `ChunkPayloadDiff`, the two-sided digest comparison diff flows report. Also builds `Vec<u8>` images. Compression framing is MCA spec knowledge, so it lives here. No `Path`, no `fs`. Names custom dimensions with util `Dimension`. |
| `sekai-core`                      | `no_std` + `alloc`                    | no (`async` only as trait bounds, see below) | Use-case policy API (`plan`/`assemble`/`commit`, `plan_checkout`) plus storage ports, over util types only. Re-exports every util type at its root, so downstream paths stay stable. `app` composes it with `anvil` for payload digests and diffs.                                                                                                                                                                               |
| `sekai-storage`                   | `std` + tokio                         | yes (RPITIT)                                 | Backend-neutral boundaries: async `BlobStore`/`MetaStore` traits (owned by `core::port`, re-exported from the `api` module), high-level `SekaiStore`/`SekaiStoreOpener` traits, and shared storage errors. Depends on `core`.                                                                                                                                                                  |
| `sekai-storage-local`             | `std` + tokio                         | yes                                          | Concrete local backend: `SqliteMeta` over sqlx plus `SledBlob` over sled, composed into `LocalStore`, both schema/version gates, and the openers. Implements the traits from `sekai-storage` (and `core::port`). Other backends become sibling crates without touching `core`/`app`.                                                                                                                  |
| `sekai-world`                     | `no_std` + `alloc`                    | no                                           | World access boundary: the `WorldTree` trait, the `RegionFile` handle, and the `WorldLock` primitives. No filesystem code.                                                                                                                                                                                                                                                                                |
| `sekai-world-local`               | `std`                                 | no                                           | Host filesystem tree: `HostWorldTree`, layout detection (vanilla, pre-26.1 Bukkit, plugin worlds), the `HostRegion` handles, the `.sekai.lock` file lock, atomic swaps, and the host `WorldError`.                                                                                                                                                                                                      |
| `sekai-app`                       | `std` + tokio                         | yes                                          | Composition/orchestration library for third parties: parallel ingest, clocks, timing, progress callbacks.                                                                                                                                                                                                                                                                                            |
| `sekai` bin (`sekai-cli` package) | `std` + tokio                         | yes                                          | Thin clap wrapper over `sekai-app`. No logic. Binary name stays `sekai`; package name stays `sekai-cli` so existing CI/artifact names keep working.                                                                                                                                                                                                                                                  |

### Why `core` must not depend on `world`

- `world` is `no_std` + `alloc` like `core`, but it already depends on
  `core` types, so a reverse edge would be a package cycle (forbidden by
  Cargo).
- `world-local` is `std` for its host context (`Path`, `fs`, `mtime`); a
  `core -> world-local` edge would poison `core` with `std` and kill
  `thumbv7m`/`wasm` portability and third-party embedding.
- Correct direction: `world-local` observes the filesystem and produces `core`
  input values (`RegionKey`) through the `WorldTree` boundary; `app` feeds
  them into `core::plan_*`. This is dependency inversion: `core` owns the shapes,
  outer crates produce them.

### The world access boundary

`world` owns the boundary between the operations and the files they
touch. A `WorldTree` names the region files it holds, derives the ones
that are missing, and hands out `RegionFile` handles: a handle reads,
sizes, fingerprints, writes, and removes one region file, and nothing
else. `sekai-world` is the boundary alone (the `WorldTree` trait, the
`RegionFile` handle, the `WorldLock` primitives); `sekai-world-local`
holds the host filesystem tree (`HostWorldTree`) behind it, mirroring
`sekai-storage`/`sekai-storage-local`. The
operations never see a path, so the flows do not depend on the tree
behind them. See
[ADR-0015](docs/adr/0015-world-region-handles.md).

### Shared value types (`sekai-util`)

Coordinates, hashes, and history records live in a leaf crate so
`anvil` and `core` name the same types without depending on each other.
The boundary rules:

- `util` holds pure data plus total validation only: no codecs, no ports,
  no orchestration. It owns the one Blake3 primitive (`hash_bytes`,
  `BytesHasher`) so every digest in the system comes from the same
  function; `anvil` builds on it instead of linking its own.
- `anvil` implements over util types (`Dimension` custom ids, `RegionKind`
  chunk families); `core` owns the storage ports and use-case policy over
  those same types, and `app` composes the two.
- `core` re-exports every util type at its root, so downstream paths
  (`sekai_core::ChunkCoord`, ...) keep working; `world`/`storage`/`app`
  continue to import through `core`.
- Codec and NBT errors (`UnknownCompression`, `CustomCompression`,
  `ExternalBody`, decompression failures, invalid chunk NBT) are `anvil`
  errors: compression framing and chunk schema knowledge both live there.

## Data Flow

```text
commit:
  world: region handle -> Vec<u8> image
    -> anvil: image -> chunk payloads (captured bytes)
    -> Chunk::normalized_hash -> ChunkHash -> storage CAS put
    -> core: merge changed chunks into the parent snapshot tree
    -> metadata: region -> dim -> root nodes + commit row

checkout:
  core::plan_checkout -> groups of (coord, ChunkHash)
    -> storage CAS fetch -> anvil builder -> Vec<u8> image
    -> world: region handle <- atomic swap(target, image)

diff:
  CAS blob or world payload -> Chunk::normalized_hash -> in-memory digest
```

A chunk has exactly one hash: the `Chunk::normalized_hash` digest of its
normalized view (see
[Two-Layer Hashing Architecture](#two-layer-hashing-architecture)). That
digest is the CAS key and the restore path, and the diff path compares the
very same digests instead of computing a second one. NBT parsing is an
implementation detail of `anvil`; no other crate can name or call it.

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
somewhere wrong, and checkout restores those files through their
discovered folders, preferring same-dimension siblings over derived
handles when folders moved since the commit.

# Data & Hashing Model

## Two-Layer Hashing Architecture

There is one chunk hash and no free function for it. `Chunk::normalized_hash`
is the only way to hash a chunk; the NBT payload forms and their normalized
views are private to `sekai-anvil`. Everything else is that digest under a
different name.

- **Normalized view** (in-memory): the decoded chunk re-encoded as NBT in a
  fixed field order per format, with the volatile `LastUpdate` and
  `InhabitedTime` tags excluded (treated as absent). Decoded fields are
  written in declaration order whatever order the file held them in, and
  the compression codec never reaches the hash. Root keys a format does not
  declare are skipped on decode and never reach the hash either, so a key
  joins change detection only once its format declares it. Compounds the
  crate does not interpret (block entities, `structures`, `blending_data`,
  legacy tile entries and ticks) are hashed as their captured bytes, so
  their inner key order does count. An absent optional compound writes
  nothing, while an empty one writes its `TAG_End`, so the two never compare
  equal. The legacy view omits its empty top-level lists and arrays; the
  modern view writes them. Each format (1.18+ root and legacy `Level`) has
  its own view, so converting a chunk is a real change, never a silent
  no-op.
- **Chunk digest -> CAS key**: `normalized_hash` through `hash_bytes` is the
  `ChunkHash`. The very same digest drives `diff` output. Because volatile
  tags never enter it, a server rewriting them neither adds a blob nor
  changes the snapshot tree; checkout still writes the captured bytes back
  verbatim.

`entities`/`poi` payloads have no level-chunk schema to normalize, so they
key on their exact bytes. That identity comes from the region kind, not from
a failed decode: the payload must still be well-formed NBT.

Nothing else gets a key. A `region/` payload that does not decode into a
level chunk - corrupt, truncated, or of a schema this version does not
read - is an error at ingest rather than a raw-byte fallback, so a damaged
chunk cannot hide behind a byte-level identity that later normalizes
differently. An unknown compression codec fails the same way.

Commit IDs hash a canonical byte encoding of the parents, creation time,
and root snapshot hash
([ADR-0027](docs/adr/0027-content-addressed-snapshot-trees.md)) with the
same `hash_bytes` primitive: a different preimage, not a second hashing
layer.

## Chunk Codec Support

Region sectors frame payloads as compression-type byte + body. Supported
vanilla codecs are `1` (Gzip), `2` (Zlib), `3` (Uncompressed), and `4` (LZ4
since 24w04a, lz4-java `LZ4BlockOutputStream` framing — not standard LZ4).
Decompression lives in `sekai-anvil`: the framing byte is MCA spec
knowledge, so `anvil` exposes `decompress_into(payload, &mut out)` and
owns all codec errors — type `127` surfaces as `CustomCompression`,
`>= 128` (external `c.<x>.<z>.mcc` body) as `ExternalBody`, anything else
as `UnknownCompression`. `Chunk::normalized_hash` decompresses the sector
payload and parses the raw NBT internally with a `no_std` parser (no
`Serialize` by design); nothing outside `sekai-anvil` sees NBT types.
Commit stores the exact captured sector payload in CAS, but derives every
key by decompressing and normalizing the chunk. Only well-formed NBT of the
kind's schema is a chunk: a `region/` payload must decode as a level chunk,
and an `entities`/`poi` payload must be a well-formed NBT compound. An
unsupported compression codec or an unparsable/unsupported payload fails
ingest; there is no raw-byte fallback.

### Chunk NBT formats

`Chunk::normalized_hash` accepts both on-disk shapes:

- **Modern** (1.18+): fields at the root, palette `sections`,
  `Heightmaps`, `Status`, plus `structures`, `PostProcessing`,
  `blending_data` and `isLightOn`, which default when absent. `Status` is
  also read under its 26.4 spelling `status`; the view always writes
  `Status`, so the rename alone is not a change, and a chunk carrying both
  spellings fails to decode.
- **Legacy** (pre-1.18): fields inside a `Level` compound, read in their
  pre-1.13 layout: per-section `Blocks`/`Add`/`Data`/`Data2` byte arrays
  (kept verbatim as owned copies, not expanded into a palette; a 1.13–1.17
  section carries a palette instead and decodes with empty arrays), a
  16-bit `HeightMap`, and
  `TerrainPopulated` mapped onto the modern `Status` vocabulary
  (`minecraft:full` / `legacy:empty` / `legacy:missing`). `DataVersion`
  is read from the root when `Level` lacks it (1.9+ writes it there).
  Decoding uses the `nanonbt` derives with `#[nbt(default)]`, because
  pre-1.9 files omit most keys; fields that own their data are required
  for that attribute, which is why the arrays are owned rather than
  borrowed.

The 1.18+ schema is decoded first; the legacy `Level` wrapper is tried
only when that read fails, so the common path pays nothing and the rare
case pays one failed pass. Each format hashes through its own normalized
view; the legacy view writes keys the modern one never does, so converting
a chunk is reported as a change rather than silently comparing equal. A
`Level` body that ends early reports `TRUNCATED_LEVEL`, which `sekai-app`
surfaces as a diagnostic naming the chunk and region file.

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

Blobs live in their own sled keyspace (`blobs/`, the default tree keyed by
the 32-byte hash) beside `meta.sqlite`, so a store is a single directory and
a blob write is durable before any metadata names it: puts append to sled's
log, and `sync` (a log flush) is the barrier the commit runs before recording
metadata. Normalization rule updates affect only change detection - zero data
migration or blob re-indexing. See
[ADR-0024](docs/adr/0024-sled-blob-store.md).

# History and State Model

## The Commit Graph

Commits form a history graph, not a timeline: every commit names the
commits it continues in `parents` (empty for the root of a history, one
entry for an ordinary commit, several for a merge). Each commit also names
its complete snapshot by root hash. The graph - not the local record order
- decides which histories a commit descends from, while its root hash
alone defines the world it restores.

Parent links are written with the commit row, in the same transaction,
and read back through the `MetaStore` port (`commit_parents`) or walked
to the root by the core use cases (`usecase::graph::commit_ancestors`
/ `commit_history`). Every parent must exist, and a parent listed twice
is recorded once.

Commit IDs are content-addressed: a Blake3 digest over the canonical
parents, creation time, and root snapshot hash. Observation metadata and
storage representation are excluded because they are not part of the
logical state. Input order never enters the hash - parents are sorted
before hashing - so equal worlds at equal times produce equal IDs, and
recording one twice is idempotent: the existing ID is returned without
duplicating rows or tree nodes. Backends keep a separate local record
sequence for traversal and latest-commit lookup; that sequence is never
part of the public ID. See
[ADR-0027](docs/adr/0027-content-addressed-snapshot-trees.md).

## Ref Objects

Ref objects are the store's movable pointers to commits: named
branches. A ref names a commit and moves with the history it names; a
ref without a commit is _unborn_. Exactly one ref is _active_ - the one
new commits hang from (the store's `HEAD`). A fresh store starts with
one active ref, `main`, unborn.

A new commit continues the target of the active ref - the port rejects
one whose parents omit it - and the active ref moves onto the new commit
in the same transaction. Switching the active ref forks the history: the
next commit continues the ref it was planned against, not the newest
commit in the store. Commit references accept a `<64-char commit hash>`
or a bare ref name.

Refs point at commit hashes. Like Git, identity is derived from content;
a recorded commit is immutable, so a ref's target keeps resolving to the
same snapshot tree. See [ADR-0019](docs/adr/0019-ref-objects.md).

## Snapshot Tree Storage

Every commit names one complete snapshot through a root hash; trees add no
second chunk-hashing layer. Three Merkle levels below the root use
`hash_bytes` with distinct domain tags: root (dimension -> dim node), dim
(region kind and coordinates -> region node), and region (local chunk
coordinates -> `ChunkHash`). Entries sort by coordinate, and a node's
stored bytes are exactly its hash preimage.

Absence is the tombstone. A chunk missing from its region node is gone,
and a region missing from its dim node is gone. Empty child nodes are never
written; a fully emptied region leaves its dim node, and an empty dim
leaves the root. The root node itself is the commit's anchor, so an empty
world stores one canonical empty root row. Reads probe root, dim, region, and slot; commit merges
only the changed subtree paths, reusing every untouched hash. The
canonical (lowest-hash) parent is the base when a merge carries several
parents; a tree merge policy is out of scope until something creates
merges.

A scoped commit merges the parent tree with its in-scope changes only:
out-of-scope subtrees are reused by hash, never re-read. Every in-scope
file is reread on commit; a chunk whose blob matches the parent writes no
new node, so an unchanged world records one commit row and reuses the
whole tree.

## Scoped Operations

Commit, checkout, and diff accept a scope: whole world (default) or
per-dimension areas — whole dimension (`--in DIM`), chunk rectangle
(`--in DIM:x0,z0..x1,z1`), explicit chunks (`--in DIM:x,z`), or region
files (`--region DIM:RX,RZ`, rectangle sugar). Region kinds (`--kind`,
repeatable; empty means all) apply uniformly across every area, and on
their own they narrow the whole world to those families (`Scope::Kinds`).
A selection can also be saved in the store as a named scope and picked
with `--scope NAME` (ADR-0021); the CLI resolves the name to the same
filter when the command runs, so commits never record which scope
produced them. The scope is a filter, not a partition:

- Scoped commit ingests and applies changes only inside the scope.
  Out-of-scope subtrees are reused by hash from the parent snapshot, never
  reread. A scoped commit therefore reads as the truth for its scope only -
  a file deleted inside the scope removes its chunks, while anything outside
  is untouched. A file the scope covers only partly records only its
  in-scope changes; the next commit that reaches it rereads the whole file
  and picks up the rest.
- Scoped checkout rebuilds and deletes only inside the scope; files
  outside are never written, deleted, or otherwise touched.
- Multi-chunk diff resolves coordinates from the commits (or world
  state) filtered by the scope and omits chunks without differences.

## Single Source of Truth

- **Source of truth**: metadata history (`commits`, `commit_parents`,
  `root_tree`, `dim_tree`, `region_tree`) in the selected DB backend + CAS
  blobs.
- **No persisted derived state**: region fingerprints are neither stored
  nor computed. Commit decisions compare chunk hashes through the snapshot
  tree, so there is no cache to wipe and no derived table to keep
  consistent. The fingerprint primitive stays for a tree-structured history
  ([ADR-0023](docs/adr/0023-drop-the-fingerprint-pass.md)).

# Storage Backends

## Trait/Impl Split (backend crates)

- `sekai-storage` owns the backend-neutral boundaries. The async
  `BlobStore`/`MetaStore` traits (RPITIT, `Send` bounds for tokio) still
  come from `core::port` and are re-exported from storage's `api` module.
  Storage adds the high-level `SekaiStore` (metadata + blobs) and
  `SekaiStoreOpener` (open/init entry points). Blob adapters are async only and
  `Clone`: the CAS is append-only, so ingest workers share it without
  exclusion and a cancelled future cannot strand a blob lock.
- `sekai-storage-local` is the concrete local backend. It owns `SqliteMeta`
  (sqlx), `SledBlob` (sled), `LocalStore`, both schemas, and both version
  gates, and implements the storage/core traits. Keeping sqlx and sled out
  of `sekai-storage` keeps the abstraction crate free of native database
  dependencies.
- New backends (mysql, postgres, embedded, in-memory, wasm) become sibling
  crates implementing the same traits. `app` is generic over
  `SekaiInstance<T: SekaiStore>`, so adding a backend does not touch use-case
  policy. No generic `Any`-driver abstraction.
- The local backend (SQLite metadata + sled blobs) is the only one shipped.
  `sekai-storage` holds no backend modules at all: it owns the traits and
  the locks, and stays free of a native database dependency. A mysql or
  postgres backend arrives as a sibling crate, never as a stub inside
  `sekai-storage`.

## Backend Selection

Compile-time, gated on the backend crate: `app` exposes a `backend-local`
feature (default-on) that pulls in `sekai-storage-local` and
`sekai-world-local`; `sekai-cli` inherits it through `sekai-app` and needs
no features of its own. The local backend is the only one shipped, and
`--store` names its root directory directly; there is no runtime URL
selection. A backend crate implements `SekaiStoreOpener` for its store
type, so the backend owns the meaning of the location string that names it.

A build without `backend-local` has no store-opening entry point at all:
`sekai-storage` ships no stand-in store, so a build that carries no backend
cannot open one and says so at compile time. The host world tree and its
`WorldError` are gated the same way; the world operations themselves stay
generic over `WorldTree` and run on any tree a caller supplies.

## Store Exclusion

Operations never lock the store. Blobs are content-addressed and
immutable, so a `put` of an existing hash is a no-op and no interleaving
of writes needs exclusion.
Metadata conflicts resolve transactionally at commit time, and content-
addressed commit IDs keep concurrent backups from colliding.

Opening the store is the one exclusive step: the blob keyspace takes a
session lock that admits a single opener per store, system-wide, for as
long as the handle lives (ADR-0024). Clones of an opened handle share
that session, so ingest workers still share the CAS without exclusion.

Only the world excludes per operation, through `WorldTree::lock`
(ADR-0014). A blocking file pass owns its share of that lock _inside_
the blocking task, so a cancelled future cannot release the world while
the pass still writes files.

## Schema

Owned by the backend crate (`crates/storage-local/schema/sqlite.sql` for
metadata; `commits`, `commit_parents`, `root_tree`, `dim_tree`,
`region_tree`, `refs`, `scopes`, `scope_areas`; `SCHEMA_VERSION` gate;
pre-release policy = recreate, don't migrate). Each tree table stores the
canonical node bytes under their hash; paths are three primary-key probes.
The blob keyspace (`blobs/`) carries its own version gate in a
reserved sled tree (`__sekai_version`); blob values are opaque payloads, so
it has no derived state to reinterpret. CAS stays behind the backend-neutral
port, so an object-store backend swaps the adapter without touching metadata
code.

## Async Policy

- `async` lives in `storage`, `storage-local`, `app`, and the bin - never in
  `anvil`/`core`/`world`/`world-local` logic. `core` uses `async fn` in traits (RPITIT,
  no `async-trait` macro, no boxing, static dispatch preserved) so
  implementations can be `tokio`-backed while policy stays runtime-free.
- Pure orchestration steps (`assemble`, image building, canonical digest)
  stay synchronous.

# Checkout & I/O Invariants

- **Atomic writes & cross-device safety**: `.mca` updates go to a temp file
  **in the same directory as the target** + `fsync` + `rename`. In-place
  mutation is prohibited. `sekai-world-local::atomic_swap` is the single owner of this
  path, reached through the region handle; `anvil` only builds the `Vec<u8>`
  image.
- **Sector alignment**: writes always land on 4 KiB boundaries with header
  offset-table integrity. Reads tolerate trailing partial sectors (e.g. torn
  tails from interrupted saves, which vanilla also opens): every referenced
  sector run is validated against exact byte bounds instead, and genuinely
  corrupt runs still fail loudly with the file path attached. `world-local` never
  patches bytes.
- **Crash consistency order**: CAS blobs flushed (sled log fsynced)
  _before_ DB metadata commit, so a torn commit leaves at most orphan
  blobs, never dangling references.
- **No server orchestration in libraries**: `save-off`/`save-all`,
  scheduling, and process control belong to callers, never to `core`,
  `anvil`, `storage`, `world`, `world-local`, or `app` internals.
- **Two-phase destructive operations** (plan before apply) hold for every
  change.
