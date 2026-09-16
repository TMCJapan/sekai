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

## Integration (`cargo bench -p sekai-app --bench backup`)

Seeded worlds from `crates/app/benches/common.rs` (`SMALL`: 2 regions,
10% density, 512 B payloads; `MEDIUM`: 8 regions, 90% density, 2 KiB
payloads). Fresh temp store per iteration.

| Bench | Median |
|---|---|
| `backup/small-full` | 14.7 ms |
| `backup/small-incremental` | 2.8 ms |
| `backup/medium-full` | 484 ms |

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
