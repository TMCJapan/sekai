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
| `commit` | report (+timings with `--timing`) | table + JSON block | regions list is timing detail, JSON-only |
| `status` | preview (+timings with `--timing`) | table + JSON block | read-only: never writes to world or store |
| `checkout` | report (+timings with `--timing`) | table + JSON block | |
| `log` | commit array (+stats with `--stat`) | n/a | no phases exist |
| `clone` | report (+timings with `--timing`) | table + JSON block | |
| `ref` | record, deletion record, or ref array | n/a | no phases exist |
| `diff` | single array, grouped array (multi), or timed object | table + JSON block | `--in`/`--region` select chunks; one chunk keeps the single shape |
| `gc` | report or dry-run plan (+timings with `--timing`) | table + JSON block | dry-run timings carry `plan_ms`; `apply_ms` is `0` |
| `prune` | report or dry-run plan (+timings with `--timing`) | table + JSON block | dry-run lists IDs; blobs are never unlinked, run `gc` after |

Without `--timing`, `--json` emits the bare result. With `--timing`,
the report that would be a bare array (`diff`) is promoted to an object
holding the array (`diffs`) plus the timing block; object reports
(commit/status/checkout/clone/gc/prune) append the block in place.

## Envelope

Every invocation prints exactly one JSON object to stdout:

```jsonc
// success
{"command": "<name>", "status": "ok", "result": <command payload>}
// failure (exit code 1)
{"command": "<name>", "status": "error", "error": "<full context chain>"}
```

`command` is one of `commit`, `status`, `checkout`, `log`, `clone`, `ref`, `prune`, `diff`, `gc`.
The canonical name is used even when an alias (`backup`, `rollback`,
`export`, `list`) invoked the command.
The error string is the full anyhow context chain (outermost message
first, then `Caused by:` lines), JSON-escaped.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success; stdout holds the `ok` envelope. |
| 1 | Runtime failure; stdout holds the `error` envelope. |
| 2 | Usage error (unknown flags, bad values). clap prints its own human-readable error to stderr. |

## Compatibility

Breaking change (PR #145): `log --json` no longer carries the `tags`
field on commit objects (tags were removed in favor of refs). Consumers
that read `tags` must be updated; the array was always empty after this
release for newly produced stores.

## Result payloads

All durations are integer milliseconds. Paths are OS strings as observed.

### `commit`

Without `--timing`:

```jsonc
{
  "commit": "3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f", "chunks": 101, "new_blobs": 5,
  "tombstones": 0, "skipped_regions": 2, "carried_chunks": 40
}
```

With `--timing` the `total_ms`/`phases`/`regions` block is appended:

```jsonc
{
  "commit": "3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f", "chunks": 101, "new_blobs": 5,
  "tombstones": 0, "skipped_regions": 2, "carried_chunks": 40, "total_ms": 123,
  "phases": {
    "discover_ms": 1, "universe_load_ms": 10, "fingerprint_ms": 2,
    "region_open_ms": 3, "ingest_ms": 40, "hash_ms": 5, "cas_put_ms": 30,
    "db_apply_ms": 20
  },
  "regions": [
    {"path": "/w/region/r.0.0.mca", "chunks": 60,
     "open_ms": 1, "ingest_ms": 20, "hash_ms": 2, "cas_ms": 15}
  ]
}
```

### `status`

```jsonc
{
  "clean": false, "latest": "2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a", "changed_regions": 1,
  "new_files": 0,
  "deleted_files": 0, "new_chunks": 3, "tombstones": 1, "new_blobs": 2,
  "total_ms": 50,
  "phases": {"discover_ms": 1, "fingerprint_ms": 2, "universe_load_ms": 3,
             "region_open_ms": 4, "ingest_ms": 30, "hash_ms": 10}
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

### `clone`

```jsonc
{
  "files_written": 2, "chunks_restored": 90,
  "total_ms": 200,
  "phases": {"plan_ms": 50, "clone_files_ms": 140}
}
```

The `total_ms`/`phases` block appears only with `--timing`.

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

`fresh_chunks`/`fresh_tombstones` count rows introduced at the
commit, `new_blobs` their distinct blobs, and `effective_chunks` the
live chunks after fallback.

### `ref`

Create (`ref create <name> [<commit>]`) and activate (`ref activate
<name>`) return the record:

```jsonc
{"name": "main", "commit": 2, "active": true, "created_at_ms": 1700000001000}
```

Deletion returns `{"name": "main"}`. `ref list` lists all refs in name
order:

```jsonc
[{"name": "feature", "commit": null, "active": false, "created_at_ms": 1700000001000},
 {"name": "main", "commit": 2, "active": true, "created_at_ms": 1700000000000}]
```

A ref created without a commit starts at the active ref's target. The
active ref is the one new commits hang from; it is never removed.

Commit arguments to `checkout`, `clone`, `diff`, `prune --before`, and
`ref create` accept `<id>` or a bare ref name; payloads always carry the
resolved numeric ID.

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
missing coordinates never error (sparse `entities`/`poi` diff empty);
only corrupt payloads and missing blobs fail loudly.

With `--timing` the array moves under `diffs` and the timing block is
appended (single and grouped alike):

```jsonc
{
  "diffs": [{"coord": {"dim": 0, "kind": 0, "x": 1, "z": 2}}],
  "total_ms": 12,
  "phases": {"blob_fetch_ms": 5, "decompress_ms": 4, "diff_compute_ms": 2}
}
```

`blob_fetch` covers commit lookup plus CAS fetch on both sides
(world-side acquisition included); `decompress` both sides'
decompression; `diff_compute` the AST diff. Commit-ID resolution and
coordinate enumeration stay outside the measured phases.

### `gc`

```jsonc
{
  "candidates": 10, "orphans": 4, "removed": 4, "total_ms": 30,
  "phases": {"plan_ms": 20, "apply_ms": 8}
}
```

The `total_ms`/`phases` block appears only with `--timing`.

Dry-run (`gc --dry-run --json`):

```jsonc
{"orphans": 4, "examined": 100}
```

With `--timing` the plan timing block is appended (`apply_ms` is
always `0`: dry-run never unlinks):

```jsonc
{
  "orphans": 4, "examined": 100, "total_ms": 20,
  "phases": {"plan_ms": 20, "apply_ms": 0}
}
```

Note: dry-run reports the orphan *count*, never the hashes.

### `prune`

```jsonc
{
  "pruned": 2, "rows_folded": 5, "rows_dropped": 3, "total_ms": 30,
  "phases": {"plan_ms": 20, "apply_ms": 8}
}
```

The `total_ms`/`phases` block appears only with `--timing`.

Dry-run (`prune --dry-run --json`) lists commit hashes instead;
`--timing` appends the plan timing block (`apply_ms` is always `0`):

```jsonc
{
  "delete": ["1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b", "2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a"],
  "retained": ["3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f", "4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e"], "total_ms": 20,
  "phases": {"plan_ms": 20, "apply_ms": 0}
}
```

Pruning never unlinks blobs: dereferenced payloads stay in CAS until
the next `gc`.

## Compatibility promise

Field names and value shapes above are stable within a major release.
Additive fields may appear; existing fields keep their types. Golden
tests across `crates/cli/src/commands/` pin every envelope and payload shape
(both bare and timing-augmented); changing them requires updating both
the tests and this document.
