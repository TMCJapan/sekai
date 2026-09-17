# Rollback

```sh
sekai --store ./sekai-store rollback ./world 1
```

Rebuilds the world from a snapshot, overwriting region files atomically
(temp file in the target directory + `fsync` + `rename`; in-place
mutation never happens). Files are stamped with the snapshot time, and
volatile tags are rewound to capture values.

The default policy is strict:

- region files unknown to the snapshot (created afterwards) are deleted;
- chunks unknown to the snapshot vanish on rebuild;
- tombstoned chunks are removed (fully tombstoned regions delete the
  file, never leaving a header-only shell);
- a blob missing from CAS aborts loudly (corruption — a partial world
  would be worse than none).

Each behavior is configurable without changing the default:

| Flag | Effect |
|---|---|
| `--keep-post-snapshot-files` | keep snapshot-unknown files instead of deleting them |
| `--keep-post-snapshot-chunks` | keep live bytes for snapshot-unknown chunks inside rebuilt regions |
| `--keep-tombstoned-chunks` | keep live bytes for tombstoned chunks; fully tombstoned files are left alone |
| `--on-missing-blob skip-chunk` | skip chunks whose blob is missing (default `abort`) |
| `--on-missing-file derived-only\|error` | ignore sibling folders, or fail, instead of guessing a location (default `sibling-first`) |

Scoped rollback rebuilds and deletes only inside the scope. Combine with
`--timing`/`--json` as usual.
