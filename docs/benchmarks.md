# Benchmarks

Micro benchmarks (criterion) and integration commit flows. Baselines
below are medians on an i3-12100F (8 threads, 31 GiB RAM), `rustc 1.98.0`,
release profile, September 2026. Your numbers will differ; compare
before/after on the same machine, never across machines.

## Micro (`cargo bench -p sekai-anvil -p sekai-core`)

| Bench | Median |
|---|---|
| `decompress/zlib-64B` | 2.4 µs |
| `decompress/zlib-8KiB` | 4.7 µs |
| `decompress/raw-64B` | 4.7 ns |
| `visit/256-chunks` | 46.1 µs |
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
| `commit/small-full` | 14.7 ms | fresh store |
| `commit/small-incremental` | 3.1 ms | second commit, no changes; measured while unchanged files were still skipped - stale since PR #151, re-measure |
| `commit/medium-full` | 484 ms | fresh store |
| `checkout/small` | 3.3 ms | ~200 chunks rewrite |
| `checkout/corpus` | 19.0 ms | 1189 chunks rewrite |
| `diff/small-all` | 72.6 ms | ~200 chunks, self-diff; per-chunk CAS opens dominate |
| `diff/corpus-all` | 655 ms | 1189 chunks, self-diff |

Rule-of-thumb estimates per subcommand (same machine class):

- `commit` full: ~500 ms for ~7K chunks; dominated by `db_apply`
  (WAL Full + per-blob fsync). A no-change commit has no file-level
  skip: it reads, decompresses, and hashes every in-scope region file,
  but reuses the whole parent tree and writes only the commit row. Nothing
  is read before ingest, and only the files to ingest are stat'd; see
  [ADR-0023](adr/0023-drop-the-fingerprint-pass.md) and
  [ADR-0027](adr/0027-content-addressed-snapshot-trees.md).
- `checkout`: rewrite-bound, ~15–20 ms per 1K chunks.
- `diff`: ~0.5 ms per chunk, dominated by per-chunk CAS file opens
  (batching opportunity, untracked).

## World-level measurement

For full-world numbers (real disks, cold/warm page cache), use the
product flags instead of criterion: `commit --timing --json` reports
per-phase `total_ms`/`phases`/`regions`. The `perf` issue template
requires this output from a release build; see
`.github/ISSUE_TEMPLATE/perf.yaml` for the required conditions
(cold/warm, runs, world size).

## CI

`bench.yaml` runs the suites on schedule and manual dispatch (never as
a merge gate). Criterion writes machine-local baselines under
`target/criterion` (uncommitted); use `--save-baseline` locally to
compare branches.
