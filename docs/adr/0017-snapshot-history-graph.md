# ADR-0017: Snapshots form a history graph

- Status: Accepted
- Date: 2026-10-21

## Context

The metadata store ordered snapshots by an auto-increment identifier and
resolved a snapshot's effective state through that order: the nearest
row with `snapshot_id <= S`. That is a timeline, and it silently assumes
the history is a single line - two histories in one store would bleed
into each other, and a snapshot could not name what it continues.

Ref objects (a movable `HEAD`, named branches) are the next step, and
they need a graph to point at.

## Decision

Every snapshot records the snapshots it continues in `parents`: a
non-null list, empty for the root of a history, one entry for an
ordinary snapshot, several for a merge. SQLite stores the edges in
`snapshot_parents`; the domain type is `Snapshot::parents`.

The graph - not the local record order - decides inheritance. The
effective state of a snapshot resolves over its ancestor set (the
snapshot plus every snapshot reachable through parent links). Of the
ancestor rows, the one recorded most recently wins; a merge snapshot
would need a distance-based tie-break, which nothing creates yet.
Backends answer the ancestor set with a recursive query over the parent
edges.

A new snapshot continues the snapshot it was planned against, which -
until ref objects exist - is the latest snapshot of the store and also
the base its delta was computed from. Merges are representable but
nothing creates them yet, so a history is a chain in practice.

Retiring a snapshot (`prune`) keeps the graph connected: the children of
the retired snapshot continue the history of its own parents, so the
surviving snapshots keep their effective state. The successor must
descend from the retired snapshot; folding into an unrelated history
would graft rows the successor never had.

The `MetaStore` port carries the graph: `create_snapshot` and
`apply_snapshot_incremental` take the parents, `snapshot_parents` reads
one node's edges, and `core::usecase::snapshot::snapshot_ancestors` /
`snapshot_history` walk back to the root. Every parent must exist and a
parent listed twice is recorded once; an unknown snapshot ID is an
error, never an empty history.

## Consequences

A store can hold several independent histories, and a snapshot never
sees rows introduced by a history it does not descend from. The
resolution costs a recursive walk per query instead of a sequence
comparison - within noise for the backup and GC flows.

Ref objects stay a follow-up: they will name a snapshot, and the graph
is what they name into. The CLI output is unchanged; the graph is
reachable through the library API.
