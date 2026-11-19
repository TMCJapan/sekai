# ADR-0037: One store version gate in conf.toml

- Status: Accepted
- Date: 2026-11-20

## Context

The local backend carried three independent version gates: `PRAGMA
user_version` in `meta.sqlite` and a reserved `__sekai_version` sled tree
in each of the `blobs/` and `cache/` keyspaces. Explicit store init
(ADR-0026) already materializes `<root>/conf.toml` and reads it before any
database opens, so a store-wide version field existed alongside the
per-database gates. Every format change had to bump and test several
constants that always moved together.

## Decision

Abolish the per-database gates and version the whole store through
`Config::version` in `conf.toml` (`CONFIG_VERSION` in
`crates/storage-local/src/config.rs`), enforced before any database opens:

- `SqliteMeta::open` creates the schema when the database holds no tables,
  re-checked under `BEGIN IMMEDIATE` so concurrent openers create it
  exactly once; it neither stamps nor reads `user_version`.
- `SledBlob` and `SledCache` open their keyspaces without a reserved
  version tree; the blob default tree stays exactly the 32-byte hashes.
- An unknown `conf.toml` version fails with `UnsupportedSchema` before
  anything opens. Pre-release policy is unchanged: recreate, never
  migrate.

## Consequences

One constant to bump and one gate to test for any format change. The
version-gate portions of ADR-0022, ADR-0024, and ADR-0029 no longer apply.
Backward compatibility is explicitly out of scope: the old per-database
version markers are ignored, not detected or migrated.
