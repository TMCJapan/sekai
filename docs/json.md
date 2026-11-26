# JSON output (`--json`)

Machine-readable contract for scripts and AI agents. Every command that
produces output accepts a per-command `--json` flag: stdout carries exactly
one JSON document, stderr stays silent on success, and colors never appear
in JSON output.

`--timing` is orthogonal to `--json`. Human mode plus `--timing` prints the
report line followed by a timing table; JSON mode plus `--timing` merges
the same timing block into the single document. Without `--timing`, JSON
carries the bare result only.

## Flag matrix

| Command | `--json` | `--timing` | Notes |
|---|---|---|---|
| `init` | store record | n/a | no phases exist |
| `commit` | report (+timings with `--timing`) | table + JSON block | regions list is timing detail, JSON-only |
| `status` | preview (+timings with `--timing`) | table + JSON block | read-only: never writes to world or store |
| `checkout` | report (+timings with `--timing`) | table + JSON block | |
| `restore` | report (+timings with `--timing`) | table + JSON block | checkout's payload; selection required |
| `log` | commit array (+stats with `--stat`) | n/a | no phases exist |
| `scope` | record, deletion record, or scope array | n/a | no phases exist |
| `cache` | clean record or update report (+timings with `--timing`) | table + JSON block (`update` only) | `clean` has no phases |
| `diff` | single array, grouped array (multi), or timed object | table + JSON block | `--in`/`--region` select chunks; one chunk keeps the single shape |
| `branch` | branch array | n/a | no phases exist |
| `repo` | record | n/a | no phases exist |

Without `--timing`, `--json` emits the bare result. With `--timing`,
the report that would be a bare array (`diff`) is promoted to an object
holding the array (`diffs`) plus the timing block; object reports
(commit/status/checkout/restore/cache update) append the block in place.

## Envelope

Every invocation prints exactly one JSON object to stdout:

```jsonc
// success
{"command": "<name>", "status": "ok", "result": <command payload>}
// failure (exit code 1)
{"command": "<name>", "status": "error", "error": "<full context chain>"}
```

`command` is one of `init`, `commit`, `status`, `checkout`, `restore`, `log`, `scope`, `cache`, `diff`, `branch`, `repo`.
The error string is the full anyhow context chain (outermost message
first, then `Caused by:` lines), JSON-escaped.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success; stdout holds the `ok` envelope. |
| 1 | Runtime failure; stdout holds the `error` envelope. |
| 2 | Usage error (unknown flags, bad values). clap prints its own human-readable error to stderr. |

## Compatibility

Change (ADR-0040): the `repo` command is added, with `create` as its
first operation. `command` gains the value `repo`; its payload is the
repository record the remote reports. See the `repo` payload below.

Change: the new `branch` command lists branches, and `command` gains
the value `branch`. See the `branch` payload below.

Change (ADR-0039): the `restore` command is added. `command` gains the
value `restore`; its payload is checkout's shape under git-restore
defaults (default source HEAD, required selection, `--overlay` keeps
source-unknown files). Existing commands are unchanged.

Change: commit arguments (`checkout`, `diff`) take a revision: a
64-character commit ID, a branch, `HEAD`, or `@`, optionally followed by
one `~<n>` (`HEAD~1`). A malformed revision is a usage error (exit
2); a revision that names nothing, such as a step past the root commit
(`unknown revision: HEAD~5`) or HEAD on an unborn branch, is a runtime
error envelope.

Breaking change: the `ref` command is removed. `command` no longer takes
the value `ref`, and the ref record, deletion record, and ref array
payloads are gone. Commits still move the active ref, and commit
arguments still accept a bare ref name.

Semantics change (ADR-0034): `commit` no longer populates the region-image
cache as part of recording; the command runs the `update_cache` pass
first, then records. In `commit --timing`, `cache_ms` now measures that
pre-commit update pass and `total_ms` spans both passes; field names and
shapes are unchanged. The app-level `commit` operation writes no cache
entries - population is exclusively `update_cache`.

Breaking change (ADR-0027): metadata schema 17 replaces delta history
with content-addressed snapshot trees. Old stores fail with an unsupported
schema error and must be recreated; commit IDs change.
`--keep-tombstoned-chunks` / `--keep-post-commit-chunks` both preserve
live chunks absent from a commit-known region (absence alone no longer
distinguishes a tombstone from a post-commit chunk).

Breaking change: the `gc` and `prune` commands are removed; commits and
blobs are append-only and the store never deletes either. `command` no
longer takes the values `gc` or `prune`, and their payloads are gone.

Breaking change (ADR-0026): store creation moves from `commit` to the new
`init` command. `commit` (and every other command except `init`) now fails
on a missing store instead of creating one; run `sekai init` first. The
envelope's `command` gains the value `init`.

Breaking change (ADR-0025): the `clone` command (alias `export`) is
removed. `command` no longer takes the value `clone`, and the clone
payload (including the `clone_files_ms` phase) is gone.

Breaking change (ADR-0023): `commit --json` no longer carries
`skipped_regions` or `carried_chunks` (always 0 since PR #151 removed
region skipping), and the `--timing` phases of `commit` and `status` no
longer carry `fingerprint_ms` (no fingerprint is computed).

Breaking change (PR #145): `log --json` no longer carries the `tags`
field on commit objects (tags were removed in favor of refs). Consumers
that read `tags` must be updated; the array was always empty after this
release for newly produced stores.

## Result payloads

All durations are integer milliseconds. Paths are OS strings as observed.
Commit arguments to `checkout` and `diff` accept `<id>` or a bare ref
name; payloads always carry the resolved numeric ID.

### `init`

```jsonc
{"store": ".sekai", "created": true}
```

`store` is the `--store` value as given. `created` is `true` when this run
materialized the store and `false` when it re-initialized an existing one
(idempotent, like `git init`). `init` registers the store itself as its
main worktree, with HEAD unborn on `main`; a re-initialization keeps
that registration and its HEAD.

### `commit`

Without `--timing`:

```jsonc
{
  "commit": "3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f", "chunks": 101, "new_blobs": 5,
  "tombstones": 0
}
```

With `--timing` the `total_ms`/`phases`/`regions` block is appended:

```jsonc
{
  "commit": "3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f", "chunks": 101, "new_blobs": 5,
  "tombstones": 0, "total_ms": 123,
  "phases": {
    "discover_ms": 1, "universe_load_ms": 10, "region_open_ms": 3,
    "ingest_ms": 40, "hash_ms": 5, "cas_put_ms": 30, "db_apply_ms": 20,
    "cache_ms": 12
  },
  "regions": [
    {"path": "/w/region/r.0.0.mca", "chunks": 60,
     "open_ms": 1, "ingest_ms": 20, "hash_ms": 2, "cas_ms": 15}
  ]
}
```

`sekai commit` runs the commit-free cache update before recording
(ADR-0034): `cache_ms` measures that update pass, `total_ms` spans both
passes, and the remaining phases and `regions` rows describe the commit
itself. With `--no-cache` the update pass is skipped: `cache_ms` is `0`,
`total_ms` spans the commit alone, and the payload shape is unchanged.

### `status`

```jsonc
{
  "clean": false, "latest": "2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a", "changed_regions": 1,
  "new_files": 0,
  "deleted_files": 0, "new_chunks": 3, "tombstones": 1, "new_blobs": 2,
  "total_ms": 50,
  "phases": {"discover_ms": 1, "universe_load_ms": 3, "region_open_ms": 4,
             "ingest_ms": 30, "hash_ms": 10}
}
```

The `total_ms`/`phases` block appears only with `--timing`.
`latest` is `null` when no commit exists yet. Counts describe what a
commit would record; nothing is written. `latest` is the commit the
preview is measured against - the target of the active ref, which is
what the next commit would continue.

### `checkout`

```jsonc
{
  "files_written": 2, "files_deleted": 1, "chunks_restored": 90,
  "total_ms": 200,
  "phases": {"plan_ms": 50, "discover_ms": 5, "checkout_files_ms": 140}
}
```

The `total_ms`/`phases` block appears only with `--timing`.

Restore strategy flags (`--keep-post-commit-files`,
`--keep-post-commit-chunks`, `--keep-tombstoned-chunks`,
`--on-missing-blob`, `--on-missing-file`) change what is written or
deleted, so the counts reflect the applied policy; the payload shape is
unchanged.

### `restore`

```jsonc
{
  "files_written": 1, "files_deleted": 0, "chunks_restored": 40,
  "total_ms": 120,
  "phases": {"plan_ms": 30, "discover_ms": 4, "checkout_files_ms": 80}
}
```

The payload is checkout's (ADR-0039): restore is a scoped checkout under
git-restore defaults, so the counts and the `total_ms`/`phases` block
(the latter only with `--timing`, phase names included) are identical.
`--overlay`, `--on-missing-blob`, and `--on-missing-file` change what is
written or deleted; the payload shape is unchanged.

### `log`

```jsonc
[{"id": "1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b", "created_at_ms": 1700000000000},
 {"id": "2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a", "created_at_ms": 1700000001000}]
```

Timestamps are raw Unix millis (RFC 3339 rendering stays human-only).
With `--stat`, per-commit change statistics are appended in place:

```jsonc
[{"id": "1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b", "created_at_ms": 1700000000000,
  "fresh_chunks": 2, "fresh_tombstones": 0, "new_blobs": 1,
  "effective_chunks": 2}]
```

`fresh_chunks`/`fresh_tombstones` count the tree diff against the
commit's first parent, `new_blobs` its distinct new present blobs, and
`effective_chunks` the live chunks in the snapshot.

### `scope`

Create (`scope create <name> (--in ... | --region ...)...`) returns the
record as stored:

```jsonc
{"name": "spawn", "areas": [{"dim": -817260936, "x0": -8, "z0": -8, "x1": 7, "z1": 7}],
 "created_at_ms": 1700000000000}
```

`areas` holds inclusive chunk rectangles with the raw integer `dim`
code (`-817260936` is the overworld), sorted by `(dim, x0, z0, x1, z1)` with duplicates removed. A
chunk (`--in DIM:x,z`) is stored as its 1x1 rectangle and a region
(`--region DIM:RX,RZ`) as its 32x32 one, so the spelling used at
creation is not kept. A replacement (`--force`) keeps the original
`created_at_ms`.

Deletion returns `{"name": "spawn"}`. `scope list` lists all scopes in
name order as an array of records.

`commit`, `status`, `checkout`, and `diff` select a saved
scope with `--scope NAME`; their payloads do not change.

### `cache`

Clean (`cache clean`) returns the number of removed entries:

```jsonc
{"entries": 3}
```

The region-image cache is derived state (ADR-0029): cleaning it only costs
performance on later commits, never changes what a commit or checkout
reads or writes, so it is always safe to run for capacity reduction. The
next commit rebuilds the entries it needs (`cache update` warms them
without a commit). `entries` is `0` when the cache was already empty;
cleaning is idempotent.

Update (`cache update [--world PATH]`) returns what the pass populated:

```jsonc
{"files": 4, "chunks": 101, "entries": 3, "existing": 1, "empty": 0}
```

With `--timing` the `total_ms`/`phases` block is appended:

```jsonc
{
  "files": 4, "chunks": 101, "entries": 3, "existing": 1, "empty": 0,
  "total_ms": 120,
  "phases": {"discover_ms": 1, "ingest_ms": 80, "hash_ms": 30, "cache_ms": 20}
}
```

`files` counts region files visited, `chunks` the chunks hashed, `entries`
the cache entries newly inserted, `existing` the entries already present
(first-image-wins kept the original), and `empty` the files holding no
chunks (they name no region node and cache nothing). `hash_ms` is a subset
of `ingest_ms`; `cache_ms` covers the writes plus the advisory flush. The
pass writes only cache entries - no CAS blobs, no metadata, no commit
(ADR-0033) - and works on a store with no commits yet. A corrupt or
unsupported chunk fails the run loudly; a cache write failure fails it too.

### `diff`

Single chunk (one `--in DIM:x,z` selection), or any selection with
changes, emits the chunk coordinates that differ (no per-tag entries):

```jsonc
[
  {"coord": {"dim": 0, "kind": 0, "x": 1, "z": 2}}
]
```

Several chunks (repeated `--in`, `--region`, or no
selection for the whole world) group entries per coordinate, omitting
chunks without differences. Rectangle selections expand fully with no
size cap: a huge rectangle enumerates every chunk inside it.

```jsonc
[
  {"coord": {"dim": 0, "kind": 0, "x": 1, "z": 2}, "entries": [
    {"path": "Status", "type": "modified", "old": "\"full\"", "new": "\"empty\""}
  ]}
]
```

`coord` uses raw integer `dim`/`kind` codes. The list contains
only chunks with differences: absent on both sides yields nothing, so
missing coordinates never error (sparse `entities`/`poi` diff empty).
A payload that has no chunk hash - corrupt, truncated, or of a schema this
version does not read - fails loudly.

With `--timing` the array moves under `diffs` and the timing block is
appended (single and grouped alike):

```jsonc
{
  "diffs": [{"coord": {"dim": 0, "kind": 0, "x": 1, "z": 2}}],
  "total_ms": 12,
  "phases": {"blob_fetch_ms": 5, "decompress_ms": 4, "diff_compute_ms": 2}
}
```

`blob_fetch` covers commit-side metadata lookup plus world-side region
acquisition; `decompress` world-side decompression and digesting (the
commit side is a recorded digest, so it contributes nothing); `diff_compute`
the digest comparison. Commit-ID resolution and coordinate enumeration stay
outside the measured phases.

### `branch`

`branch` returns an array of every branch in full refname order:

```jsonc
[{"name": "refs/heads/dev", "commit": "1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b", "head": false},
 {"name": "refs/heads/main", "commit": null, "head": true}]
```

`name` is the full refname. `commit` is `null` while the branch is
unborn: a fresh store lists `main` that way, before its first commit.
`head` is `true` on HEAD's branch. There are no patterns; filter with
`jq` (`.result[] | select(.name | startswith("refs/heads/feature/"))`).

### `repo`

Create (`repo create <organization/name> --remote <URL> [-d <text>]`)
returns the repository record the remote reports:

```jsonc
{"organization": "my-org", "name": "my-project", "description": "My new repository", "created": true}
```

`description` is `null` when none was given. `created` is `true` when
this run created the repository and `false` when it already existed
(idempotent, like `git init`); the remote worker currently always
reports `true` because creation is stubbed (ADR-0040). A remote
rejection (non-2xx) is a runtime error envelope carrying the remote's
`{"code", "message"}` body; an address outside the `organization/name`
grammar or a remote URL that is not http(s) is a usage error (exit 2).

## Compatibility promise

Field names and value shapes above are stable within a major release.
Additive fields may appear; existing fields keep their types. Golden
tests across `crates/cli/src/commands/` pin every envelope and payload shape
(both bare and timing-augmented); changing them requires updating both
the tests and this document.
