# ADR-0012: The region fingerprint hashes the whole file

- Status: Accepted
- Date: 2026-09-26

## Context

Incremental backup decides a file is unchanged when its `(mtime, size,
header_hash)` triple matches the stored `region_state` row. `header_hash`
covered only the first 4 KiB - the location table - so every chunk payload
byte past that was invisible, leaving mtime as the sole guard for content.

mtime is not a content signal. `cp -p`, `rsync -t`, `tar -x`, a ZFS/Btrfs
snapshot rollback, or any filesystem with coarse timestamps all place
changed bytes at an unchanged mtime. A same-size payload edit (block placed,
player moved, inventory changed) leaves the location table byte-identical
because the compressed length did not move, so all three signals matched and
the file was declared unchanged. The backup stored nothing, and the next
`rollback` overwrote the newer bytes with the older ones it had captured -
silent data loss with no error anywhere. This is the same reason restic
refuses to trust mtime alone.

## Decision

Hash the entire file. `fingerprint_file` streams it in 128 KiB chunks
through a new `anvil::ContentHasher`, so memory stays bounded regardless of
region size, and the field is renamed `content_hash` (schema version 6;
derived state, so a mismatch costs one full re-ingest and never wrong data).

The incremental path becomes read-bound: it reads every region file instead
of 4 KiB per file. Measured on an i3-12100F, warm cache, release
(`docs/benchmarks.md`): `backup/small-incremental` 2.7 ms -> 3.1 ms, and on
the 9.6 MiB corpus a no-change backup goes `total=9ms`/`fp=0ms` ->
`total=14ms`/`fp=5ms`. Cold-cache cost is a sequential read of the world,
which the ingest path pays anyway when anything changed.

Rejected: sampling the header plus a few sectors (still misses edits in the
middle, so it buys cost without correctness); and a size+mtime+header fast
path with a full verify behind a flag (a flag nobody sets is the bug again).

## Consequences

A same-size payload edit under a preserved mtime is now ingested, pinned by
`fingerprint_sees_a_payload_edit_with_preserved_size_and_mtime`. Plans that
stored a 4 KiB header hash do not match the new column, so the first backup
after upgrading re-ingests once - derived state, by design. `debug scan`
reports the same digest under its new name, and no reader of a store has to
care: nothing outside `region_state` depends on it.
