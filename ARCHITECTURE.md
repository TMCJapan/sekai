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
  atomic checkout, and GC.
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
      -> sekai-anvil (no_std + alloc: pure sector codec, decompress, NBT normalization)
    -> sekai-world (std: filesystem owner - discover/observe/swap)
      -> sekai-anvil + sekai-core (types only)
    -> sekai-storage (std + tokio: store traits and locks)
      -> sekai-core
    -> sekai-storage-sqlite (std + tokio: sqlx SQLite implementation)
      -> sekai-storage
    -> sekai-storage-surrealdb (std + tokio: SurrealDB/SurrealKV implementation)
      -> sekai-storage
sekai-util (no_std, no tokio dependency: coordinates, chunk hashes,
  history value types) <- anvil, core (world/storage/app reach them
  through `core` re-exports)
```

| Crate | `std` | `async` | Role |
|---|---|---|---|
| `sekai-util` | `no_std`, no `std`/tokio dependencies | no | Shared value types: coordinates, the chunk digest (`ChunkHash`, `hash_bytes`, `BytesHasher`), region/history/commit/gc records, hex errors. Pure data plus total validation only. |
| `sekai-anvil` | `no_std` + `alloc` | no | Pure Anvil sector codec including decompression and chunk NBT normalization. A stored sector payload becomes a `Chunk` whose `normalized_hash()` is the only public NBT-derived digest; the parser and normalized views stay internal. Also builds `Vec<u8>` images. Compression framing is MCA spec knowledge, so it lives here. No `Path`, no `fs`. Names custom dimensions with util `Dimension`. |
| `sekai-core` | `no_std` + `alloc` | no (`async` only as trait bounds, see below) | Use-case policy API (`plan`/`assemble`/`commit`, `plan_checkout`, `gc plan`/`apply`) plus storage ports. Composes `anvil` over util types and re-exports every util type at its root, so downstream paths stay stable. |
| `sekai-storage` | `std` + tokio | yes (RPITIT) | Backend-neutral boundaries: async `BlobStore`/`MetaStore` traits (owned by `core::port`, re-exported from the `api` module), high-level `SekaiStore`/`SekaiStoreOpener` traits, and URL-based backend selection. Depends on `core`. |
| `sekai-storage-sqlite` | `std` + tokio | yes | Concrete SQLite implementation over sqlx: `SqliteMeta`/`SqliteBlob` plus the SQLite-backed `SqliteStore`, both schema/version gates, and the opener. Implements the traits from `sekai-storage` (and `core::port`). Other backends become sibling crates without touching `core`/`app`. |
| `sekai-storage-surrealdb` | `std` + tokio | yes | Concrete embedded SurrealDB implementation over SurrealKV: one `SurrealdbStore` serves metadata and blobs, owns the schema/version gate, and implements the storage traits plus `SekaiStoreOpener`. Default backend. |
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
  input values (`RegionKey`); `app` feeds them into `core::plan_*`. This is dependency inversion: `core` owns the shapes,
  outer crates produce them.

### The world access boundary

`world` owns the boundary between the operations and the files they
touch. A `WorldTree` names the region files it holds, derives the ones
that are missing, and hands out `RegionFile` handles: a handle reads,
sizes, fingerprints, writes, and removes one region file, and nothing
else. The
operations never see a path, so the flows do not depend on the tree
behind them. See
[ADR-0015](docs/adr/0015-world-region-handles.md).

### Shared value types (`sekai-util`)

Coordinates, hashes, and history records live in a leaf crate so
`anvil` can name the same types as `core` without a package cycle
(`core -> anvil -> core` is forbidden by Cargo). The boundary rules:

- `util` holds pure data plus total validation only: no codecs, no ports,
  no orchestration. It owns the one Blake3 primitive (`hash_bytes`,
  `BytesHasher`) so every digest in the system comes from the same
  function; `anvil` builds on it instead of linking its own.
- `anvil` implements over util types (`Dimension` custom ids, `RegionKind`
  chunk families); `core` composes it (storage ports, use cases).
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

- **Normalized view** (in-memory): the chunk encoded canonically - compound
  keys sorted, big-endian scalars, UTF-8 strings, empty collections omitted -
  with the volatile `LastUpdate` and `InhabitedTime` tags excluded (treated
  as absent). It never hashes re-serialized on-disk NBT bytes, so it is
  independent of key order and of the compression codec. Each format (1.13+
  and legacy `Level`) has its own view, so converting a chunk is a real
  change, never a silent no-op.
- **Chunk digest -> CAS key**: `normalized_hash` through `hash_bytes` is the
  `ChunkHash`. The very same digest drives `diff` output. Because volatile
  tags never enter it, a server rewriting them neither adds a blob nor
  records a history row; checkout still writes the captured bytes back
  verbatim.

`entities`/`poi` payloads have no level-chunk schema to normalize, so they
key on their exact bytes. That identity comes from the region kind, not from
a failed decode: the payload must still be well-formed NBT.

Nothing else gets a key. A `region/` payload that does not decode into a
level chunk - corrupt, truncated, or of a schema this version does not
read - is an error at ingest rather than a raw-byte fallback, so a damaged
chunk cannot hide behind a byte-level identity that later normalizes
differently. An unknown compression codec fails the same way.

Commit IDs hash a canonical byte encoding of the logical delta
([ADR-0020](docs/adr/0020-content-addressed-commit-ids.md)) with the same
`hash_bytes` primitive: a different preimage, not a second hashing layer.

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

- **Modern** (1.13+): fields at the root, palette `sections`,
  `Heightmaps`, `Status`.
- **Legacy** (pre-1.13): fields inside a `Level` compound,
  per-section `Blocks`/`Add`/`Data`/`Data2` byte arrays (kept verbatim as
  owned copies, not expanded into a palette), a 16-bit `HeightMap`, and
  `TerrainPopulated` mapped onto the modern `Status` vocabulary
  (`minecraft:full` / `legacy:empty` / `legacy:missing`). `DataVersion`
  is read from the root when `Level` lacks it (1.9+ writes it there).
  Decoding uses the `nanonbt` derives with `#[nbt(default)]`, because
  pre-1.9 files omit most keys; fields that own their data are required
  for that attribute, which is why the arrays are owned rather than
  borrowed.

The 1.13+ schema is decoded first; the legacy `Level` wrapper is tried
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

Blobs are content-addressed and durable before any metadata names them.
The SQLite backend keeps them in their own `blob.sqlite` database beside
`meta.sqlite`, so a store is a single directory of database files; see
[ADR-0022](docs/adr/0022-sqlite-blob-store.md). The SurrealDB backend keeps
the `blob` table in the same embedded SurrealKV database as metadata, so a
store is one SurrealKV directory. Normalization rule updates affect only
change detection - zero data migration or blob re-indexing.

# History, State, and GC Model

## The Commit Graph

Commits form a history graph, not a timeline: every commit names the
commits it continues in `parents` (empty for the root of a history, one
entry for an ordinary commit, several for a merge). The graph - not the
local record order - decides what a commit inherits, so a commit never
sees rows introduced by a history it does not descend from.

Parent links are written with the commit row, in the same transaction,
and read back through the `MetaStore` port (`commit_parents`) or walked
to the root by the core use cases (`usecase::graph::commit_ancestors`
/ `commit_history`). Every parent must exist, and a parent listed twice
is recorded once.

Commit IDs are content-addressed: a Blake3 digest over the canonical
parents, creation time, and fresh history rows (present or tombstone).
Observation metadata and storage representation are excluded because they
are not part of the logical delta. Input order never enters the hash -
parents and entries are sorted before hashing - so equal commits produce
equal IDs, and recording one twice is idempotent: the existing ID is
returned without duplicating rows. Backends keep a separate local record
sequence for traversal, latest-commit lookup, and fold ordering; that
sequence is never part of the public ID. See
[ADR-0020](docs/adr/0020-content-addressed-commit-ids.md).

## Ref Objects

Ref objects are the store's movable pointers to commits: named
branches. A ref names a commit and moves with the history it names; a
ref without a commit is *unborn*. Exactly one ref is *active* - the one
new commits hang from (the store's `HEAD`). A fresh store starts with
one active ref, `main`, unborn.

A new commit continues the target of the active ref - the port rejects
one whose parents omit it - and the active ref moves onto the new commit
in the same transaction. Switching the active ref forks the history: the
next commit continues the ref it was planned against, not the newest
commit in the store. Commit references accept a `<64-char commit hash>`
or a bare ref name.

Refs point at commit hashes. Like Git, identity is derived from content;
unlike Git, `prune` may fold a retired commit's rows into a retained
successor. Folding changes where rows physically live, never the
successor's logical delta or ID, so refs stay stable across a
prune. See [ADR-0019](docs/adr/0019-ref-objects.md).

One rule follows from the graph:

- **Pruning folds into a descendant.** A retired commit hands its rows
  to a retained commit that continues it, and a ref naming the retired
  commit follows the fold. A commit that several branches continue
  cannot be retired - folding it would strip the other branches of the
  rows they inherit through it - so `prune` refuses and names it.

Retiring a commit (`prune`) keeps the graph connected: the children of
the retired commit continue the history of its own parents, so the
surviving commits resolve to the same effective state as before.

## Delta History Storage

Commits store only fresh rows (changed chunks plus tombstones).
Unchanged chunks have no row at newer commits; reads resolve effective
state through the graph - `lookup_chunk` returns the row introduced at
the commit or at one of its ancestors (the nearest one wins),
`visit_commit_chunks` synthesizes one effective row per known
coordinate. Every in-scope file is reread on commit; a row is written
only when its blob differs from the base commit. Every commit therefore
grows metadata with changes, not with universe size - an unchanged commit
writes one commit row and no chunk rows.

## Tombstones

Missing chunks are explicit `NULL`-blob rows, written once at the commit
where they vanish: a coordinate already tombstoned records nothing further,
because its absence already resolves through fallback. Change detection
therefore compares the world against the effective *present* coordinate set
of the latest commit, so a repeated commit of an unchanged world writes no
chunk rows at all. Checkout onto tombstones removes sectors (fully
tombstoned regions delete the file rather than leaving a header-only shell).

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

- Scoped commit ingests and tombstones only inside the scope.
  Out-of-scope coordinates record no rows and no tombstones, resolving
  through fallback from earlier commits. A scoped commit therefore reads
  as the truth for its scope only - a file deleted inside the scope
  tombstones its chunks, while anything outside is untouched. A file the
  scope covers only partly records only its in-scope changes; the next
  commit that reaches it rereads the whole file and picks up the rest.
- Scoped checkout rebuilds and deletes only inside the scope; files
  outside are never written, deleted, or otherwise touched.
- Multi-chunk diff resolves coordinates from the commits (or world
  state) filtered by the scope and omits chunks without differences.

## Single Source of Truth

- **Source of truth**: metadata history (`commits`, `commit_parents`,
  `chunk_history`) in the selected DB backend + CAS blobs.
- **No persisted derived state**: region fingerprints are neither stored
  nor computed. Commit decisions compare per-chunk history, so there is
  no cache to wipe and no derived table to keep consistent. The
  fingerprint primitive stays for a tree-structured history
  ([ADR-0023](docs/adr/0023-drop-the-fingerprint-pass.md)).

## Garbage Collection (GC)

- **Reference scope**: global (all commits/dimensions/chunks), matching
  global CAS dedup.
- **Two-phase safety**: `gc plan` (read-only candidates) then `gc apply`
  (re-verified unlink). No metadata writes in the orphan-only scope.
- **Exclusivity**: `gc` keeps to its own run marker (ADR-0011): a run
  that writes blobs holds `commit.inflight` in the store root for its
  duration, and `gc` refuses at plan and apply time while it is held,
  because a commit's blobs are in the CAS before the rows that reference
  them. Readers (`checkout`, `clone`, `diff`) never take it. Operations
  otherwise never lock the store; exclusion is per world, through
  `WorldTree::lock` (ADR-0022).
- **Commit pruning** (`prune`): oldest-first fold into the retained
  commit that continues the retired one, then delete. A commit that
  several branches continue cannot be retired, so `prune` refuses and
  names it. Pruning dereferences blobs only; `gc` reclaims them
  afterwards.

# Storage Backends

## Trait/Impl Split (backend crates)

- `sekai-storage` owns the backend-neutral boundaries. The async
  `BlobStore`/`MetaStore` traits (RPITIT, `Send` bounds for tokio) still
  come from `core::port` and are re-exported from storage's `api` module.
  Storage adds the high-level `SekaiStore` (metadata + blobs) and
  `SekaiStoreOpener` (URL selection). Blob adapters are async only and
  `Clone`: the CAS is append-only, so ingest workers share it without
  exclusion and a cancelled future cannot strand a blob lock.
- `sekai-storage-surrealdb` is the default embedded backend. One
  `SurrealdbStore` implements both `BlobStore` and `MetaStore` over a
  SurrealKV database; `SekaiStore` exposes the same handle as both adapters.
  It owns the table/index schema and the `SCHEMA_VERSION` gate.
- `sekai-storage-sqlite` is the optional sqlx backend. It owns `SqliteMeta`,
  `SqliteBlob`, `SqliteStore`, both schemas, and both version gates, and
  implements the storage/core traits.
- New backends (mysql, postgres, embedded, in-memory, wasm) become sibling
  crates implementing the same traits. `app` is generic over
  `SekaiInstance<T: SekaiStore>`, so adding a backend does not touch use-case
  policy. No generic `Any`-driver abstraction.
- `sekai-storage` holds no backend modules at all: it owns the traits and the
  locks, and stays free of a native database dependency. A mysql or postgres
  backend arrives as a sibling crate, never as a stub inside `sekai-storage`.

## Backend Selection

Runtime-selected, with compile-time gating on the backend crate: `app`
exposes `backend-surrealdb` (default-on, pulls in
`sekai-storage-surrealdb`) and `backend-sqlite` (pulls in
`sekai-storage-sqlite`); `sekai-cli` inherits the default through
`sekai-app` and needs no features of its own. Bare paths select the default
SurrealDB backend; `surrealkv://` and `surrealdb://` name it explicitly, and
`sqlite://` names SQLite. Requesting a backend this build does not carry is
a loud `UnsupportedBackend` error — never silent reinterpretation.
`sekai-storage` keeps `BackendKind` and URL parsing; each backend crate
implements `SekaiStoreOpener` for its store type, so the backend owns the
grammar of the URLs that name it.

A build without either backend feature has no store-opening entry point at
all: `sekai-storage` ships no stand-in store, so a build that carries no
backend cannot open one and says so at compile time.

## Store Exclusion

Operations never lock the store. Blobs are content-addressed and
immutable, so a `put` of an existing hash is a no-op and no interleaving
of writes needs exclusion; `gc` keeps to its own run marker (ADR-0011).
Metadata conflicts resolve transactionally at commit time, and content-
addressed commit IDs keep concurrent backups from colliding.

Only the world excludes, through `WorldTree::lock` (ADR-0014). A
blocking file pass owns its share of that lock *inside* the blocking
task, so a cancelled future cannot release the world while the pass
still writes files.

## Schema

Owned by the backend crate. SQLite uses
`crates/storage-sqlite/schema/sqlite.sql` (`commits`, `commit_parents`,
`chunk_history`, `refs`, `scopes`, `scope_areas`) and a separate blob
database (`blob.sqlite`, one `blobs` table) with its own `user_version`
gate. SurrealDB defines `meta`, `blob`, `commit`, `chunk_history`, `ref`, and
`scope` tables in `crates/storage-surrealdb/src/surrealdb.rs`, recording
`SCHEMA_VERSION` in `meta:store`. Both backends follow the pre-release
policy: recreate, don't migrate. Blob rows are opaque payloads, so they have
no derived state to reinterpret. CAS stays behind the backend-neutral port,
so an object-store backend swaps the adapter without touching metadata code.

## Async Policy

- `async` lives in `storage`, `world`, `app`, and the bin - never in
  `anvil`/`core` logic. `core` uses `async fn` in traits (RPITIT,
  no `async-trait` macro, no boxing, static dispatch preserved) so
  implementations can be `tokio`-backed while policy stays runtime-free.
- Pure orchestration steps (`assemble`, image building, canonical digest)
  stay synchronous.

# Checkout & I/O Invariants

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
  - **Write path**: CAS blob rows committed (WAL fsynced) *before* DB
    metadata commit; the checkpoint afterwards manages WAL space, not
    durability.
  - **GC delete path**: DB metadata commit *before* blob unlink.
  - **Prune path**: each commit's fold (row restamp/drop and commit
    delete) commits atomically; blobs unlink only in a later `gc`.
- **No server orchestration in libraries**: `save-off`/`save-all`,
  scheduling, and process control belong to callers, never to `core`,
  `anvil`, `storage`, `world`, or `app` internals.
- **Two-phase destructive operations** (plan before apply) hold for every
  change.
