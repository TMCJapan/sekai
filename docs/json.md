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
| `backup` | report (+timings with `--timing`) | table + JSON block | regions list is timing detail, JSON-only |
| `rollback` | report (+timings with `--timing`) | table + JSON block | |
| `list` | snapshot array | n/a | no phases exist |
| `diff` | single array, grouped array (multi), or timed object | table + JSON block | `--in`/`--region` select chunks; one chunk keeps the single shape |
| `gc` | report or dry-run plan (+timings with `--timing`) | table + JSON block | dry-run timings carry `plan_ms`; `apply_ms` is `0` |
| `debug scan` | entry array (+timings with `--timing`) | table + JSON block | |

Without `--timing`, `--json` emits the bare result. With `--timing`,
reports that would be bare arrays (`diff`, `scan`) are promoted to an
object holding the array (`diffs`/`entries`) plus the timing block;
object reports (backup/rollback/gc) append the block in place.

## Envelope

Every invocation prints exactly one JSON object to stdout:

```jsonc
// success
{"command": "<name>", "status": "ok", "result": <command payload>}
// failure (exit code 1)
{"command": "<name>", "status": "error", "error": "<full context chain>"}
```

`command` is one of `backup`, `rollback`, `list`, `diff`, `gc`, `scan`.
The error string is the full anyhow context chain (outermost message
first, then `Caused by:` lines), JSON-escaped.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success; stdout holds the `ok` envelope. |
| 1 | Runtime failure; stdout holds the `error` envelope. |
| 2 | Usage error (unknown flags, bad values). clap prints its own human-readable error to stderr. |

## Result payloads

All durations are integer milliseconds. Paths are OS strings as observed.

### `backup`

Without `--timing`:

```jsonc
{
  "snapshot": 3, "chunks": 101, "new_blobs": 5, "tombstones": 0,
  "skipped_regions": 2, "carried_chunks": 40
}
```

With `--timing` the `total_ms`/`phases`/`regions` block is appended:

```jsonc
{
  "snapshot": 3, "chunks": 101, "new_blobs": 5, "tombstones": 0,
  "skipped_regions": 2, "carried_chunks": 40, "total_ms": 123,
  "phases": {
    "discover_ms": 1, "universe_load_ms": 10, "fingerprint_ms": 2,
    "region_open_ms": 3, "ingest_ms": 40, "hash_ms": 5, "cas_put_ms": 30,
    "db_apply_ms": 20
  },
  "regions": [
    {"path": "/w/region/r.0.0.mca", "bytes": 12345, "chunks": 60,
     "open_ms": 1, "ingest_ms": 20, "hash_ms": 2, "cas_ms": 15}
  ]
}
```

### `rollback`

```jsonc
{
  "files_written": 2, "files_deleted": 1, "chunks_restored": 90,
  "total_ms": 200,
  "phases": {"plan_ms": 50, "discover_ms": 5, "rollback_files_ms": 140}
}
```

The `total_ms`/`phases` block appears only with `--timing`.

Restore strategy flags (`--keep-post-snapshot-files`,
`--keep-post-snapshot-chunks`, `--keep-tombstoned-chunks`,
`--on-missing-blob`, `--on-missing-file`) change what is written or
deleted, so the counts reflect the applied policy; the payload shape is
unchanged.

### `list`

```jsonc
[{"id": 1, "created_at_ms": 1700000000000}, {"id": 2, "created_at_ms": 1700000001000}]
```

Timestamps are raw Unix millis (RFC 3339 rendering stays human-only).

### `diff`

Single chunk (one `--in DIM:x,z` selection):

```jsonc
[
  {"path": "Status", "type": "modified", "old": "\"full\"", "new": "\"empty\""},
  {"path": "xPos", "type": "added", "val": "3"},
  {"path": "old_tag", "type": "removed", "val": "1b"}
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

`type` is `added` (`val`), `removed` (`val`), or `modified`
(`old`+`new`). Values are SNBT strings, always complete (human
`--show-values` truncation does not apply here). `coord` uses raw
integer `dim`/`kind` codes, matching `scan`.

A chunk absent (or tombstoned) on a side diffs as an empty compound
there: absent on both sides yields no entries, present on one side
reports whole-value `added`/`removed` entries for every leaf. Missing
coordinates are never errors (sparse `entities`/`poi` diff empty);
only corrupt payloads and missing blobs fail loudly.

With `--timing` the array moves under `diffs` and the timing block is
appended (single and grouped alike):

```jsonc
{
  "diffs": [{"path": "xPos", "type": "added", "val": "3"}],
  "total_ms": 12,
  "phases": {"blob_fetch_ms": 5, "decompress_ms": 4, "diff_compute_ms": 2}
}
```

`blob_fetch` covers snapshot lookup plus CAS fetch on both sides
(world-side acquisition included); `decompress` both sides'
decompression; `diff_compute` the AST diff. Snapshot-ID resolution and
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

### `scan` (`debug scan`)

```jsonc
[
  {"path": "/w/region/r.0.0.mca", "dim": 0, "kind": 0,
   "region_x": 0, "region_z": 0, "size": 12345, "mtime_ms": 1700000000000,
   "chunks": 60, "header_hash": "ab12..."},
  {"path": "/w/region/r.1.0.mca", "dim": 0, "kind": 0,
   "region_x": 1, "region_z": 0, "size": 8192, "mtime_ms": null,
   "chunks": 0, "header_hash": "cd34..."}
]
```

`dim`/`kind` are raw integer codes; `mtime_ms` is `null` when the
platform cannot provide a timestamp.

With `--timing` the array moves under `entries` and the timing block is
appended:

```jsonc
{
  "entries": [],
  "total_ms": 30,
  "phases": {"discover_ms": 1, "read_ms": 20, "parse_ms": 8}
}
```

## Compatibility promise

Field names and value shapes above are stable within a major release.
Additive fields may appear; existing fields keep their types. Golden
tests across `crates/cli/src/commands/` pin every envelope and payload shape
(both bare and timing-augmented); changing them requires updating both
the tests and this document.
