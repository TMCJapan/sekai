# ADR-0007: checked-in binary test-world corpus

- Status: Accepted
- Date: 2026-09-16

## Context

Synthetic fixtures cannot surprise us: hand-built NBT never exercises
real compression variety, fragmentation, or sparse `entities`/`poi`
layouts. Options were an external world generator (SteelMC: AGPL-3.0
vs our Apache-2.0, whole-server weight, no entities), a seeded
self-built generator, or real files.

## Decision

Both: `test-world/` holds three audited real files (~10 MiB total:
full 1024-chunk region, 151-chunk entities, 14-chunk poi) plus a
provenance README (mob-only entities, no player data); tests copy it
to temp dirs, never mutate in place. Parameterized scale worlds come
from the seeded generator (`crates/app/benches/common.rs`).
Micro benchmarks use criterion; integration flows run through it too;
`docs/benchmarks.md` pins baselines. `bench.yaml` runs scheduled only.

## Consequences

Real-data regression (`real_world_corpus_round_trip`) plus
reproducible scale benches. `CONTRIBUTING.md` records the corpus as
the sole exception to the synthetic-fixture rule. Corpus swaps require
re-audit and count updates in the test.
