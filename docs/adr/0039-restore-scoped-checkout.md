# ADR-0039: Restore is a scoped checkout under git-restore defaults

- Status: Accepted
- Date: 2026-11-24

## Context

`checkout` rebuilds the world (or a selection) from an explicitly named
commit, but the everyday hot-recovery case is narrower: undo local
damage in one area against the commit the world is currently on, without
naming it and without touching anything outside the area. Git splits
exactly this case out of `checkout` into `restore` (ADR-0018 adopted
git's vocabulary), and users arriving from git expect the same split
here. The constraints: sekai has no index, the checkout machinery
already owns plan-then-apply rebuilding, and the JSON contract gains a
new command envelope.

## Decision

Add `sekai restore <world> [--source <rev>] [--overlay] <selection>`
and `SekaiInstance::restore`, composed over the existing checkout:

- The selection plays git's pathspec and is **required**: an empty
  selection (or `--kind` alone, which spells no path) is refused with
  git's wording, "you must specify path(s) to restore". Whole-world
  rebuilding stays checkout's job.
- `--source` / `-s` names the restore source (any revision `checkout`
  accepts). Omitted, it defaults to the active ref's target (`HEAD`):
  git restores from the index by default, and sekai has no index, so
  HEAD is the only meaningful default. An unborn active ref fails with
  `NoCommits`. Git's `--staged`/`--worktree` locations do not apply:
  the live world is always the restore location.
- `--overlay` maps to git restore's overlay mode: keep region files
  inside the selection that the source does not know. The default is
  git's no-overlay mode, which deletes them to match the source
  exactly.
- Restored files are rebuilt wholesale, like git overwrites a worktree
  file's content: chunks unknown to the source vanish even under
  overlay, so checkout's `--keep-post-commit-chunks` and
  `--keep-tombstoned-chunks` have no restore analog and stay off.
  `--on-missing-blob` and `--on-missing-file` carry over unchanged as
  rebuild mechanics; `--jobs` sizes the instance's runner, exactly as
  checkout's does.
- The API is `SekaiInstance::restore(&mut self, world, source:
  Option<&Revision>, scope, RestoreOptions, progress)`: it resolves the
  source, maps `RestoreOptions` onto `CheckoutOptions`
  (`keep_post_commit_files = overlay`, both keep-chunk policies off),
  and runs `WorldHandleMut::checkout`. The report and timings are
  checkout's, and the JSON payload reuses checkout's shape under the
  `"restore"` envelope (phase names included).

Nothing is recorded and no ref moves; the world lock, plan-before-apply
order, and failure/cancellation guarantees are checkout's, inherited
whole.

## Consequences

The common recovery run needs no commit ID and cannot accidentally
widen to the whole world. The cost is one more command surface that
must stay aligned with checkout's mechanics; keeping restore a thin
composition (no second rebuild path) is what holds that line. No schema
change: restore reads the same metadata and CAS a checkout reads.
`docs/json.md` documents the shared payload shape.
