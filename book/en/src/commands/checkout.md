# Checkout

```sh
sekai checkout ./world 1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b
sekai checkout ./world @stable --in overworld
```

Rebuilds the world from a commit, overwriting region files atomically
(temp file in the target directory + `fsync` + `rename`; in-place
mutation never happens). The commit argument accepts a 64-character commit hash or `@tag`
(see [Tags](tags.md)). Files are stamped with the commit time, and
volatile tags are rewound to capture values.

The default policy is strict:

- region files unknown to the commit (created afterwards) are deleted;
- chunks unknown to the commit vanish on rebuild;
- tombstoned chunks are removed (fully tombstoned regions delete the
  file, never leaving a header-only shell);
- a blob missing from CAS aborts loudly (corruption — a partial world
  would be worse than none).

Each behavior is configurable without changing the default:

| Flag | Effect |
|---|---|
| `--keep-post-commit-files` | keep commit-unknown files instead of deleting them |
| `--keep-post-commit-chunks` | keep live bytes for commit-unknown chunks inside rebuilt regions |
| `--keep-tombstoned-chunks` | keep live bytes for tombstoned chunks; fully tombstoned files are left alone |
| `--on-missing-blob skip-chunk` | skip chunks whose blob is missing (default `abort`) |
| `--on-missing-file derived-only\|error` | ignore sibling folders, or fail, instead of guessing a location (default `sibling-first`) |

- Scope flags (`--in`, `--region`, `--kind`) rebuild and delete only
  inside the scope; see [Scope selection](../scope.md). Combine with
  `--timing`/`--json` as usual.
- Before rebuilding, the CLI echoes the target commit, scope, and
  policy to stderr (human output only).
