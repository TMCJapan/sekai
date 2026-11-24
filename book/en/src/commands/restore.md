# Restore

```sh
sekai restore ./world --in overworld:-8,-8..7,7
sekai restore ./world --source HEAD~1 --scope spawn
sekai restore ./world -s main --overlay --region overworld:0,0
```

Restores the selected portion of the world from a source commit —
sekai's `git restore`. Where [Checkout](checkout.md) rebuilds the world
(or a selection) from a commit you name, restore is the everyday
hot-recovery case: undo local damage in one area against the commit the
world is currently on, without naming it. Nothing is recorded and no
ref moves; region files are rebuilt atomically from the captured
payloads, exactly like checkout.

## Source and selection

- The selection plays git's pathspec and is **required**: spell areas
  with `--in`/`--region`, or name a saved scope with `--scope` (see
  [Scope selection](../scope.md)). A bare `sekai restore ./world` — or
  `--kind` alone, which narrows families but spells no path — fails
  with `you must specify path(s) to restore`. Whole-world rebuilding
  stays checkout's job.
- `--source <rev>` / `-s <rev>` names the restore source: any revision
  checkout accepts (a commit hash, a ref name, `HEAD`/`@`, optionally
  one `~<n>` step). Omitted, it defaults to HEAD's commit
  (`HEAD`) — sekai has no index, so HEAD replaces git's index default.
  An unborn HEAD (no commit to continue) fails with `no commit
  selected`.

## What gets written

The default follows git restore's no-overlay mode:

- region files inside the selection that the source does not know
  (created afterwards) are deleted, to match the source exactly;
- every rebuilt file holds exactly the source's chunks: chunks created
  after the source commit vanish, and tombstoned chunks are removed
  (fully tombstoned regions delete the file);
- a blob missing from CAS aborts loudly, like checkout.

Each behavior is configurable without changing the default:

| Flag | Effect |
|---|---|
| `--overlay` | keep source-unknown files instead of deleting them (git's overlay mode); rebuilt files still hold exactly the source's chunks |
| `--on-missing-blob skip-chunk` | skip chunks whose blob is missing (default `abort`) |
| `--on-missing-file derived-only\|error` | ignore sibling folders, or fail, instead of guessing a location (default `sibling-first`) |

- `--jobs N` sets how many files rebuild at once (`0` means one per
  CPU).
- Combine with `--timing`/`--json`/`--progress` as usual; the JSON
  payload is checkout's shape under the `restore` envelope (see
  [Automation](../automation.md)).
- Before rebuilding, the CLI echoes the source, scope, and policy to
  stderr (human output only).

Pause the server's writes first, exactly like checkout; see
[Server coordination](../server-coordination.md).
