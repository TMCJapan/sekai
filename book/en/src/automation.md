# Automation

Every command accepts `--json`: stdout carries exactly one JSON document,
stderr stays silent on success, and colors never appear.
`commit`, `status`, `checkout`, `diff`, and
`cache update` accept `--timing`, which prints a per-phase table in
human mode and merges the same block into the JSON document in `--json`
mode. Progress is off with `--json`, and `--progress` is refused with
it.

Exit codes: `0` success, `1` runtime failure (stdout holds the error
envelope), `2` usage error (clap prints to stderr).

Dry runs by command:

| Preview | Dry-run means |
|---|---|
| `status` | the whole command (commit has no `--dry-run` by design) |
| `checkout` | none (checkout is destructive by definition) |

The full contract — flag matrix, envelope, per-command payloads, and the
compatibility promise — is [docs/json.md](https://github.com/tmcjapan/sekai/blob/main/docs/json.md). That page
is the reference; this book does not duplicate it.
