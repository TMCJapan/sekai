# ADR-0005: mmap rejected for reads and SQLite

- Status: Accepted
- Date: 2026-09-13

## Context

Candidate: `memmap2` file reads (region files) and `PRAGMA mmap_size`
(metadata) to cut the `region_open`/`universe_load` phases. Adoption
bar was ≥10% on `ingest`/`universe` phases, else skip.

## Decision

Rejected. Measured on a 65 MiB / 16-region / 16K-chunk world (release,
n=5 interleaved medians, warm and cold page cache): mmap zeroed
`region_open` (76→0 ms) but page faults relocated into ingest, leaving
`ingest`/`total` within ±2%; `PRAGMA mmap_size` (256 MiB, verified
active per-connection) moved `universe_load` within noise (±8%,
direction inconsistent). The 610 KiB metadata DB is fully cached; the
phase is query-bound, not I/O-bound. All measurement scaffolding
(`ImageBytes`, env switches, `memmap2` dep) was reverted; zero
production residue.

## Consequences

Reads stay `fs::read`, SQLite keeps default `mmap_size`. The actual
bottleneck is `db_apply` (~600 ms, WAL Full + per-blob fsync) — a
write-path project with its own crash-safety design, tracked
separately. Revisit only with a workload where reads dominate.
