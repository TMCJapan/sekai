# ADR-0018: Git-style command and domain vocabulary

- Status: Accepted
- Date: 2026-10-22

## Context

The CLI spoke its own dialect: `backup` recorded a "snapshot", `rollback`
restored one, `export` copied one, and `list` enumerated them. Git already
owns that vocabulary with different names (`commit`, `checkout`, `clone`,
`log`), so every user arriving from git had to learn a second mental model
for the same four operations, and the docs had to explain the translation.

The old words also leaked everywhere: the library API (`Snapshot`,
`SnapshotId`, `BackupOptions`), the storage schema (`snapshots`,
`snapshot_parents`), the run marker (`backup.inflight`), and the JSON
contract (`{"command":"backup"}`, `"snapshot": 3`). A rename therefore
touches every layer at once - or it never happens, and the vocabulary keeps
drifting.

## Decision

Adopt git's vocabulary for the operations *and* for the history entity, and
keep the old command names as visible aliases so existing scripts and
muscle memory keep working.

| Canonical | Alias | Operation |
|---|---|---|
| `commit` | `backup` | record the current world state as a new commit |
| `status` | - | preview what a commit would record (no writes) |
| `checkout` | `rollback` | rebuild the live world from a commit |
| `log` | `list` | list recorded commits, oldest first |
| `clone` | `export` | rebuild a commit into a fresh directory |
| `tag` | - | give commits human-readable names |
| `diff` | - | compare chunk NBT between commits (or the world) |
| `prune` | - | retire old commits, folding their rows into retained ones |
| `gc` | - | collect orphan blobs |

The aliases are clap `visible_alias`es: they parse, and the help prints
them next to the canonical name. The two flags renamed with the domain
vocabulary keep hidden long aliases
(`--keep-post-snapshot-files`, `--keep-post-snapshot-chunks`), so a script
that still calls the old command with the old flags keeps working.

The domain follows the same vocabulary: `Snapshot`/`SnapshotId`/
`SnapshotTag`/`SnapshotEntry`/`SnapshotStats` become `Commit`/`CommitId`/
`CommitTag`/`CommitEntry`/`CommitStats`; the graph-read use case module
`usecase::snapshot` becomes `usecase::graph`; the record step
`usecase::backup::commit` becomes `usecase::commit::record_commit` (a
module and a function may not share a name); the app API becomes
`WorldHandleMut::commit`, `WorldHandleMut::checkout`, and
`SekaiInstance::clone_to` - `clone_to`, not `clone`, so the call can never
be mistaken for `Clone::clone`.

The storage schema follows as well: `snapshots` -> `commits`,
`snapshot_parents` -> `commit_parents`, `snapshot_tags` -> `commit_tags`,
and the `snapshot_id` columns of `chunk_history` and `region_state` become
`commit_id`. `SCHEMA_VERSION` goes from 8 to 9, and the pre-release policy
applies: old stores are recreated, never migrated.

The run marker becomes `commit.inflight` (ADR-0011 keeps its decision).

The JSON contract follows the canonical names: the envelope carries
`"command":"commit"` (an alias invocation still reports the canonical
name), the commit report carries `"commit": 3` instead of `"snapshot": 3`,
a tag record carries `"commit": 2`, and the timing phases are
`checkout_files_ms` and `clone_files_ms`.

## Consequences

Existing invocations keep working: `sekai backup ./world` is `sekai commit
./world`, and the help says so. Scripts that parse the JSON payload must
follow (`snapshot` -> `commit`); `docs/json.md` and the golden tests in
`crates/cli/src/commands/` change together.

Stores written by earlier builds fail the version gate and must be
recreated; the schema change is not migrated, by pre-release policy.

The vocabulary is now git's, so git-shaped features (refs, branches, a
movable HEAD) read naturally when they arrive. The domain still has no ref
objects: a commit continues the commit it was planned against, and
histories stay chains (ADR-0017).

Earlier ADRs use the pre-rename words (`backup`, `snapshot`). Their
decisions stand; this ADR supersedes their wording only.
