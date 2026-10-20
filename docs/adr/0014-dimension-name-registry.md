# ADR-0014: Dimension names resolve through a store registry

- Status: Accepted
- Date: 2026-10-09

## Context

Dimensions were keyed by an opaque `i32`: `0..=2` for the vanilla trio
and a truncated Blake3 hash of the root-relative path for everything
else. The hash can collide (aliasing two dimensions onto one timeline)
and cannot be read back as a name. A proposed alternative (PR #97) makes
`Dimension` a `Cow<str>` and widens `chunk_history.dim` /
`region_state.dim` to `TEXT`, which keeps identity readable and
collision-free but is a breaking change across every crate: `Dimension`,
`ChunkCoord`, `RegionKey`, and the history records stop being `Copy`,
and the schema breaks.

The project is pre-release (stores are recreated, not migrated), so a
schema break is affordable - but the compile-time blast radius of
de-stringifying the whole domain model is not the only option.

## Decision

Dimension identity lives in a store-side registry:

- A `dimensions(id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT UNIQUE)`
  table maps canonical names (`minecraft:overworld`, `aether:sky`,
  `./plugin-folder`) to stable integer codes. The vanilla trio is seeded
  with `0..=2`, so `Dimension(i32)` and every `INTEGER` history/state
  column keep their meaning and `Dimension` stays a `Copy` newtype;
  custom names get the next auto-increment code and can never alias the
  reserved vanilla codes.
- The `MetaStore` port owns resolution, not a concrete `Store` type:
  `lookup_dimension(&self, name)` is the read-only name -> code
  translation, and `resolve_dimension(&mut self, name)` registers a name
  on first sight (idempotent upsert). Backends implement both; callers
  that cannot depend on `sekai-storage` pass any `MetaStore` through the
  existing trait, keeping the dependency flow inward.
- Names are canonical and opaque to the store; normalization
  (`namespace:path` / `./folder`) stays a caller concern.
- `SCHEMA_VERSION` bumps 6 -> 7 for the added table. Pre-release policy
  applies: existing stores fail loudly and are recreated.

## Consequences

Readable, collision-free dimension identity without touching the
`i32`-shaped domain types or the existing history columns; the only
schema cost is an additive lookup table. `Dimension` codes are now
store-assigned instead of path-hashed, so the same id is stable across
folder moves as long as callers pass the same canonical name, and
`debug`-style output needs a registry lookup to render names (out of
scope here). This ADR is groundwork: wiring discovery and the CLI to the
registry is a follow-up, and it supersedes the string-key approach of
PR #97 if landed.
