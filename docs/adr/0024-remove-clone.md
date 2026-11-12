# ADR-0024: Remove the clone command

- Status: Accepted
- Date: 2026-11-12

## Context

`clone` (alias `export`, ADR-0018) rebuilt a commit into a fresh, empty
directory without touching the live world. It is no longer needed.

Several pieces existed only to serve it: the `SekaiInstance::clone_to`
library API with its `CloneOptions`/`CloneReport`/`CloneTimings`, the
`--flavor`/`--base` output-layout flags, the `WorldTree::is_empty` check
that guarded the empty target, and the `HostWorldTree::new_dimensions`/
`HostWorldTree::new_bukkit` constructors that built output trees for roots
that did not exist yet.

## Decision

Remove the command and everything that existed only for it:

- CLI: the `clone` subcommand and its `export` alias, `CloneFlavor`, and
  the `--flavor`/`--base` flags.
- App: `SekaiInstance::clone_to`, `CloneOptions`, `CloneReport`, and
  `CloneTimings`.
- World: `WorldTree::is_empty`, `HostWorldTree::new_dimensions`, and
  `HostWorldTree::new_bukkit`. `HostWorldTree::new_legacy` stays: it is
  the fallback `HostWorldTree::new` resolves to when no layout is
  detected.
- JSON contract (ADR-0002): `clone` leaves the envelope's `command` set,
  and its payload and the `clone_files_ms` phase are gone.

Earlier ADRs that mention `clone` or its earlier name `export` (0003,
0009, 0011, 0013, 0014, 0015, 0018, 0021) stay as written: they record the
decisions of their time.

## Consequences

`sekai clone ...` and `sekai export ...` now fail at parse time; scripts
that called them must drop the call. `WorldTree` implementations outside
this workspace lose one method to implement, and an implementation that
still defines `is_empty` inside its `impl WorldTree` block stops compiling
until the method is removed or moved to an inherent `impl`.
