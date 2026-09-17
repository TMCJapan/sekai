# Automation

Every command accepts `--json`: stdout carries exactly one JSON document,
stderr stays silent on success, and colors never appear. Every command
except `list` and `tag` accepts `--timing`, which prints a per-phase table in human
mode and merges the same block into the JSON document in `--json` mode.
`--progress` draws a stderr bar and is refused with `--json`.

Exit codes: `0` success, `1` runtime failure (stdout holds the error
envelope), `2` usage error (clap prints to stderr).

Dry runs by command:

| Preview | Dry-run means |
|---|---|
| `status` | the whole command (backup has no `--dry-run` by design) |
| `gc --dry-run`, `prune --dry-run` | plan only, nothing deleted |
| `rollback`, `export` | none (rollback is destructive by definition; export targets an empty directory, so it is trivially reversible) |

The full contract — flag matrix, envelope, per-command payloads, and the
compatibility promise — is [docs/json.md](../../docs/json.md). That page
is the reference; this book does not duplicate it.
