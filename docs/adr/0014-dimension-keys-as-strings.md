# ADR-0014: Dimension keys are strings, not hashed codes

- Status: Accepted
- Date: 2026-10-09

## Context

`Dimension` was an `i32` code: `0..=2` for the vanilla trio and a Blake3
digest truncated to `i32` for everything else (`custom_dimension_id`,
plus a remap for hashes that landed on vanilla codes). Custom
`dimensions/<ns>/<name>` trees and arbitrary plugin-world folders were
therefore keyed by a hash of their root-relative path.

Two problems follow. First, a truncated 32-bit hash can collide: two
distinct dimensions could silently share a timeline, which is wrong data,
not a cache miss. The remap only protected the three reserved vanilla
codes, not the custom space. Second, the hash is opaque: `debug scan`
printed integers nobody could map back to a dimension, and the tool could
not exploit the fact that the game itself identifies dimensions by the
unique `namespace:path` resource location (`minecraft:overworld`,
`aether:sky`).

The project is pre-release: stores are recreated, not migrated, so a
schema break carries no data-migration cost.

## Decision

`Dimension` is a string key.

- Official dimensions keep their namespaced id: `minecraft:overworld`,
  `minecraft:the_nether`, `minecraft:the_end`, and
  `dimensions/<ns>/<name>` trees become `<ns>:<name>` (`aether:sky`).
- Layout-derived dimensions - plugin worlds and nested copies, which have
  no official id - use their root-relative folder path with a `./` prefix
  (`./sky`, `./sky/DIM-1`). The prefix keeps folder keys disjoint from
  namespaced ids by construction; the path is unique within a scanned
  tree.
- `anvil::custom_dimension_id` and `core::resolve_custom_dimension` are
  deleted; `anvil` no longer depends on `sekai-util`.
- `util::Dimension` wraps a `Cow<'static, str>` with validation
  (`DimensionError`); vanilla aliases (`overworld`, `nether`, `end`,
  `DIM-1`, `DIM1`, ...) parse to official ids, bare unknown names fail.
- CLI `--in`/`--region` peel the area suffix from the right (a trailing
  `X,Z` or `X0,Z0..X1,Z1`), so namespaced ids keep their colons.
- Metadata schema: `chunk_history.dim` and `region_state.dim` become
  `TEXT`; `SCHEMA_VERSION` bumps to 7 (recreate, don't migrate).
- JSON `dim` fields (`diff`, `debug scan`) become strings.

## Consequences

Dimension identity is now the game's own unique id where one exists, so
same-id data can never silently fork and collisions are impossible; a
hash collision can no longer alias two dimensions. `debug scan` and
`diff --json` expose readable keys, and selecting a custom dimension with
`--in aether:sky` or `--in ./sky/DIM-1` works directly.

Costs: `Dimension` is no longer `Copy`, so `ChunkCoord`, `RegionKey`,
`SnapshotEntry`, and history/fingerprint records lost `Copy` too (clones
now appear at ownership boundaries); metadata rows allocate the key
string per decode; and the store schema, CLI surface, and JSON `dim`
shape changed pre-release without migration. Because the id is the key,
two folders claiming the same custom id (`world/dimensions/aether/sky`
and `sky/dimensions/aether/sky`) now resolve to one dimension with a
deterministic discovery order instead of separate hashed histories.
