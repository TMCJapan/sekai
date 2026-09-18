# ADR-0009: Snapshot tags and `@tag` references

- Status: Accepted
- Date: 2026-09-18

## Context

Numeric snapshot IDs are stable but unreadable in runbooks; aliases
must survive shells, paths, and JSON without quoting and never collide
with ID syntax.

## Decision

`TagName` allows ASCII `[A-Za-z0-9._-]` up to 64 bytes, never all
digits, so `@123` always means snapshot 123. `rollback`, `export`,
`diff`, and `prune --before` accept `<id>` or `@tag`; payloads carry
the resolved numeric ID. Duplicate names fail unless `--force` moves
the tag; tags vanish with their snapshot on prune (cascade). Bare
`tag` lists, `tag -d` deletes.

## Consequences

Automation can pin `@stable` instead of counting IDs. Renames are
explicit moves, never silent overwrites.
