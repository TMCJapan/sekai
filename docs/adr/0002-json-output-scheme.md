# ADR-0002: JSON output scheme

- Status: Accepted
- Date: 2026-09-08

## Context

Machine-readable output grew organically: a global `--porcelain` flag
conflicting with per-command `--timing-json`/`--json`, plus bare-array
outputs. Flag interaction was order-dependent and the schema had two
parallel shapes.

## Decision

One scheme: per-command `--json`, orthogonal `--timing`. Every
invocation prints exactly one JSON envelope
(`{"command","status","result"|"error"}`); without `--timing` the
result is the bare report, with it the timing block merges in (arrays
promote to `{"diffs"|"entries",...}` objects). Removed flags fail
loudly as unknown arguments. Serialization lives in CLI-local DTOs;
domain structs never derive output shapes, and `nbt::Value` has no
`Serialize` impl by design (derived form would compete with SNBT).

## Consequences

Contract pinned in `docs/json.md` with byte-identical golden tests.
Scripts target one shape per command. Human tables stay untouched.
