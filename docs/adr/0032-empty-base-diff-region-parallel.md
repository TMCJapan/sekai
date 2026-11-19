# ADR-0032: Empty-base world diff and region-parallel diff pipeline

- Status: Amended by ADR-0036 (`diff` with an unborn HEAD fails on the
  command line; `app` still diffs against the empty snapshot)
- Date: 2026-11-16

## Context

Two gaps surfaced while diffing a full world (13,460 region files, ~2.4M
chunks) against a freshly initialized store:

1. `diff --world` failed outright on an empty store: the CLI resolved the
   active ref before scanning and errored with `NoCommits` when the ref was
   unborn. Yet "what would the first commit add?" is exactly the question a
   diff against nothing answers, and ADR-0006 already treats a missing side
   as empty rather than as an error.
2. The world side of the diff read each region file once *per requested
   chunk*: a full-world diff re-read and re-parsed every multi-megabyte file
   hundreds of times, and chunk enumeration decoded whole files just to
   learn which slots are occupied. Single-threaded decode put a full-world
   diff far beyond interactive use.

Constraints: digests must stay byte-identical to the CAS keys ingest
records (no hash change); output stays in input coordinate order; progress
steps once per region file; corrupt payloads still fail loudly, naming the
chunk and file; `core`/`storage` stay `no_std`; the region-image cache
proofs (ADR-0031) keep their semantics.

## Decision

- **Empty base**: `diff_world_chunks(None, ..)` resolves the active ref as
  before, but an unborn ref now means the empty snapshot instead of an
  error: every present chunk reports as added. `None` keeps meaning "the
  active ref"; only unborn→error became unborn→empty. The CLI passes the
  unresolved `Option` through; an explicit reference that fails to resolve
  still errors before the world is scanned. Commit-vs-commit diff is
  untouched: it still requires two real commits.
- **Header-only enumeration**: presence lives in the location table, so
  `RegionFile::visit_present` answers "which chunks exist" from an 8 KiB
  header read. The host tree overrides it; the default keeps the full-read
  walk for trees that cannot do better. The table walk is a superset of the
  decoded walk: a corrupt entry is still named and fails loudly when its
  payload is read.
- **Region-parallel world side**: coordinates are grouped by region,
  keeping their input positions. Each unproven file is read once on the
  blocking pool and digests only its requested chunks; results scatter back
  into input positions as tasks complete, so output order never depends on
  completion order. The fan-out is bounded like ingest but oversubscribed
  (2× CPU): the work is decode-dominated, so extra tasks keep every core
  busy while others wait on reads. Each task holds a share of the world
  lock until it ends (ADR-0022). Proven regions (ADR-0031) still answer
  both sides from the committed node without a read; a region whose file is
  gone leaves its coordinates absent. Both step progress at once.
- **Bulk commit side**: above 1,024 unproven coordinates the recorded
  digests come from one materialized tree walk instead of a metadata lookup
  per chunk; below it, point lookups stay cheaper.
- **SIMD inflate**: `miniz_oxide`'s `simd` feature (SSE2 baseline on
  x86_64, `no_std`-compatible) roughly halves decompression cost. Checksum
  verification stays on: skipping it would trade safety for speed.

Timings keep their meaning: reads and metadata lookups under `blob_fetch`,
decompression and digesting under `decompress`, digest comparison under
`diff_compute`. The per-phase values are sums across workers, so they can
exceed the wall-clock total.

## Consequences

- `init` + full-world `diff` works end to end: ~2.4M added chunks reported
  in well under a minute on a 6-core laptop, where the previous code errored
  immediately and, had it not, would have re-read every file per chunk.
- A single-chunk diff still touches only its own region: one header read
  plus one file read, never a whole-world scan, and the bulk tree walk never
  triggers below the threshold.
- A file-level read failure now fails the diff instead of being confined to
  one coordinate: one read answers many coordinates, so there is no honest
  per-coordinate attribution left. Chunk-level decode failures keep naming
  their chunk and file.
- Out of scope: streaming the normalized NBT straight into the hasher
  (avoids one buffer copy) and faster coordinate rendering; the current
  margin does not need them.