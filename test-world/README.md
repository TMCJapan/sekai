# test-world corpus

Small real-data world excerpt for regression tests and benchmarks.
Tests must copy these files into a temp dir first; never mutate them
in place (rollback rewrites region files).

## Contents

All three files cover overworld region `(1, 1)`:

| Path | Size | Chunks | Notes |
|---|---|---|---|
| `region/r.1.1.mca` | 9.0 MiB | 1024 | Full region (ancient city / deep dark terrain), zlib |
| `entities/r.1.1.mca` | 696 KiB | 151 | Villagers, animals, minecarts |
| `poi/r.1.1.mca` | 68 KiB | 14 | Villager profession POIs |

Total: 1189 chunks.

## Provenance

Extracted from a vanilla Java Edition server world (bot-populated test
server, no real players). Audited 2026-09: the entities file contains
mob entities only — no `CustomName`, no player data, no plugin markers;
`UUID` hits are tag names (mob UUIDs are binary int arrays, not PII).
The region file's `carpet` hits are `minecraft:*_carpet` block names,
`PlayerRange`/`waiting_for_players`/`starlight.light_version` are
vanilla spawner/lighting strings, not player data.

## Regeneration

These files are static snapshots, not generated. Parameterized scale
worlds (size, density, fragmentation, snapshot depth) are produced by
the seeded generator under `crates/app/benches/` instead; use that for
benchmarks needing shapes this corpus does not cover.
