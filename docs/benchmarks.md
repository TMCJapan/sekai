# Benchmarks

Micro benchmarks (criterion) and integration backup flows. Baselines
below are medians on an i3-12100F (8 threads, 31 GiB RAM), `rustc 1.98.0`,
release profile, September 2026. Your numbers will differ; compare
before/after on the same machine, never across machines.

## Micro (`cargo bench -p sekai-anvil -p sekai-nbt -p sekai-core`)

| Bench | Median |
|---|---|
| `decompress/zlib-64B` | 2.4 µs |
| `decompress/zlib-8KiB` | 4.7 µs |
| `decompress/raw-64B` | 4.7 ns |
| `visit/256-chunks` | 46.1 µs |
| `parse/4-sections` | 410 ns |
| `parse/64-sections` | 6.8 µs |
| `diff/64-sections` | 0.47 µs |
| `display/64-sections` | 4.3 µs |
| `digest/64-sections` | 10.0 µs |
| `hash/64B` | 55 ns |
| `hash/4KiB` | 1.37 µs |
| `hash/1MiB` | 253 µs |

## Integration (`cargo bench -p sekai-app`)

Seeded worlds from `crates/app/benches/common.rs` (`SMALL`: 2 regions,
10% density, 512 B payloads; `MEDIUM`: 8 regions, 90% density, 2 KiB
payloads) plus the checked-in `test-world/` corpus (1189 chunks).
Fresh temp store per iteration, except `-incremental` and the
read-only flows.

| Bench | Median | Notes |
|---|---|---|
| `backup/small-full` | 14.7 ms | fresh store |
| `backup/small-incremental` | 2.8 ms | second backup, no changes |
| `backup/medium-full` | 484 ms | fresh store |
| `rollback/small` | 3.3 ms | ~200 chunks rewrite |
| `rollback/corpus` | 19.0 ms | 1189 chunks rewrite |
| `diff/small-all` | 72.6 ms | ~200 chunks, self-diff; per-chunk CAS opens dominate |
| `diff/corpus-all` | 655 ms | 1189 chunks, self-diff |
| `gc-plan/small` | 2.1 ms | read-only plan, repeatable |
| `scan/corpus` | 8.2 ms | read-only, 10 MiB world |

Rule-of-thumb estimates per subcommand (same machine class):

- `backup` full: ~500 ms for ~7K chunks; dominated by `db_apply`
  (WAL Full + per-blob fsync). Incremental no-change runs near
  `universe_load` cost.
- `rollback`: rewrite-bound, ~15–20 ms per 1K chunks.
- `diff`: ~0.5 ms per chunk, dominated by per-chunk CAS file opens
  (batching opportunity, untracked).
- `gc plan`: a few ms at small scale; grows with history length.
- `debug scan`: disk-read bound, ~1 ms per MiB here.

## World-level measurement

For full-world numbers (real disks, cold/warm page cache), use the
product flags instead of criterion: `backup --timing --json` reports
per-phase `total_ms`/`phases`/`regions`. The `perf` issue template
requires this output from a release build; see
`.github/ISSUE_TEMPLATE/perf.yaml` for the required conditions
(cold/warm, runs, world size).

## CI

`bench.yaml` runs the suites on schedule and manual dispatch (never as
a merge gate). Criterion writes machine-local baselines under
`target/criterion` (uncommitted); use `--save-baseline` locally to
compare branches.
