# ADR-0015: The world boundary hands out region handles, not paths

- Status: Accepted
- Date: 2026-10-20

## Context

`sekai-app` used to receive the on-disk path of every region file:
discovery returned `BTreeMap<RegionKey, PathBuf>`, and the operations
opened, hashed, swapped, and deleted through those paths
(`sekai_world::open_image`, `fingerprint_file`, `atomic_swap`). The path
was the only currency between the world layer and the composition layer,
so every operation was a filesystem operation by construction: a world
that is not stored on disk - a browser build, an embedded server, a test
fixture - could not be backed by the same flows.

A path is also more capability than an operation needs. It names a file
anywhere on the host, which is why the rollback path had to re-derive
sibling paths (`sekai_world::sibling_path`) to restore a region into a
folder that had moved since the backup.

## Decision

The world boundary hands out handles, not paths. `WorldTree` names the
region files it holds and derives the ones that are missing
(`derive_region`); each handle is a `RegionFile` that reads, observes its
fingerprint, writes a rebuilt image, and removes the file - nothing else.
Where a handle lives and which file counts as its sibling
(`sibling_region`) stay with the tree that produced it.

`HostWorldTree` is the filesystem tree: its handle owns a path it never
hands to the operations, and the atomic swap stays inside the world
crate. A tree that stores its region files elsewhere implements the same
two traits and names its own error type.

Failures keep their context: `WorldError` is the world layer's error type
and carries the path, so the operations no longer rebuild error messages
from paths themselves.

## Consequences

A tree outside this workspace implements `WorldTree` plus one `RegionFile`
handle and names its own error type - no `Path` in sight. The composition
layer holds handles instead of paths, so `RegionTiming` reports a location
string rather than a path, and the `--timing` JSON output is unchanged.

The cost: the host tree's error type changed from `std::io::Error` to
`WorldError`, the sibling lookup moved from a free function into the tree,
and export and rollback now go through the handle instead of calling
`world::atomic_swap` themselves.
