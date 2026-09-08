# Overview & System Boundaries

## Purpose
A Chunk-level deduplicated backup and rollback tool for Minecraft region files (`.mca`). Designed for frequent hot-recovery rather than long-term archival.

## Scope & Boundaries
- **In Scope**: MCA parsing, chunk payload extraction, NBT normalization for diffing, Content-Addressable Storage (CAS), MVCC metadata management, atomic rollback, and GC.
- **Out of Scope**: External process lifecycle, server I/O synchronization (`save-off`, `save-all`, `save-on`), and cron/scheduling. These responsibilities belong exclusively to caller software or administrators.

## Core Abstraction & Isolation Policy
To guarantee pure, deterministic domain logic, `crates/core` is strictly **`no_std` and zero-dependency**.
- **I/O & Persistence Isolation**: Storage engines (CAS/SQLite in `storage`) and File I/O (`mca`) implement traits defined in `core`.
- **Codec Isolation**: NBT parsing, normalization, and Gzip decompression/compression are delegated to `nbt` and `mca`, driven by `engine`. `core` only works with abstract chunk coordinates, identifiers, and byte/hash views.

# Data & Hashing Model

## Two-Layer Hashing Architecture
To reproduce the exact raw payloads captured for each history entry while avoiding false-positive changes from non-essential tags (e.g., `LastUpdate`), hashing is split into two distinct layers:

- `blob_hash` (CAS Key):
  - Hash computed over the exact raw byte payload (as read from the MCA file).
  - Serves as the immutable key in the CAS blob store. It identifies the exact raw payload captured for a history entry: rollback reproduces those captured bytes verbatim, including volatile tags (e.g., `LastUpdate`) exactly as they were at capture time — so rolling back also rewinds ignored tags to the snapshot's capture point.
- `diff_hash` (Volatile Diff View):
  - Hash computed from uncompressed NBT payload with non-essential tags excluded (treated as absent, which is change-detection-equivalent to zero-clearing while staying type-agnostic).
  - The digest runs over a canonical encoding (compound keys sorted, big-endian scalars, UTF-8 strings), never over re-serialized NBT bytes, so it is independent of on-disk key order and compression codec.
  - V1 rules exclude `LastUpdate`. Rule updates only change which names are skipped.
  - Used *only* in-memory or in cache tables to detect meaningful game-state changes. Never alters stored blobs.
  - The `chunk_history.diff` column is reserved as that cache, but no producer populates it yet: backup always records `NULL` (not computed) and the normalizer has no caller in the hot path, so every row reads back as "not computed" until a consumer lands.

## Chunk Codec Support
Region sectors frame payloads as compression-type byte + body. Supported vanilla codecs are `1` (Gzip), `2` (Zlib), `3` (Uncompressed), and `4` (LZ4 since 24w04a). Type `4` uses the lz4-java `LZ4BlockOutputStream` framing, which is *not* the standard LZ4 block/frame format. Codec rejection lives in the NBT diff-view path (`sekai-nbt`): type `127` (third-party custom codec) surfaces as `CustomCompression`, values `>= 128` (body stored externally in `c.<x>.<z>.mcc`) as `ExternalBody`, and anything else unknown as `UnknownCompression`. Backup itself never validates codecs: it ingests raw sector payloads opaquely into CAS, so an unsupported codec only surfaces when a diff view is derived, never as an ingest failure.

## CAS Consistency
Normalization rules affect only change-detection logic. Updating normalization rules in future versions requires zero data migration or blob re-indexing.

# History, State, and GC Model

## MVCC & Interval Coverage
Chunk histories are tracked independently per chunk coordinate (`dim`, `kind`, `chunk_x`, `chunk_z`).
- History entries represent state over snapshot intervals `[S_i, S_{i+1})`.
- The dataset is treated as an MVCC timeline rather than a Git-like commit DAG.

## Tombstones
A missing chunk in a snapshot (unexplored/deleted area) is explicitly tracked via `NULL` blob references (Tombstones). Rollback to such snapshots must truncate/remove corresponding sectors in the target MCA file.

## Single Source of Truth vs Derived State
- **Single Source of Truth**: Metadata history (`snapshots`, `chunk_history`) and CAS storage (`blobs/`).
- **Derived State**: Transient indices (e.g., `region_state` file fingerprints) maintained for quick change detection. Derived state can be completely dropped at any time: fingerprints are re-observed from the live world files on the next backup (they are not recoverable from history alone, since `mtime`/`size`/header hashes are never stored in `chunk_history`), so wiping `region_state` degrades the next backup to a full ingest, never to wrong data.

## Garbage Collection (GC)
- **Reference Scope**: GC checks referential integrity across the entire database (all chunks, dimensions, and snapshots), as CAS deduplication is global.
- **Two-Phase Safety**: GC offers a dry-run phase (`gc plan`) before physical deletion (`gc apply`) at the `engine` library seam. The CLI does not expose either phase yet, so collection is currently library-only.
- **Current Scope (orphan-only)**: GC reclaims blobs referenced by no history row and touches no metadata rows; snapshot pruning does not exist yet, so there is no DB commit to order the unlinks against.

# Rollback & I/O Invariants

- **Atomic Writes & Cross-Device Safety**: Updates to `.mca` files must be written to a temporary file **located in the same directory as the target `.mca` file** (to guarantee `fs::rename` atomicity across different filesystems/mount points) before swapping into place. Direct in-place binary mutation of live region files is prohibited.
- **Sector Alignment**: Writes to `.mca` files must strictly enforce 4KiB sector boundaries and header offset table integrity.
- **Crash Consistency Order**:
  - **Write Path**: CAS Blobs MUST be flushed and fsynced to disk *before* committing DB metadata.
  - **GC Delete Path**: once snapshot pruning exists, its DB metadata MUST be committed *before* physically unlinking CAS Blobs. Today's orphan-only GC performs no metadata writes, so the ordering is vacuous until pruning lands.

