# ADR-0009: Snapshot tags and `@tag` references (superseded: tag concept removed)

- Status: Superseded (tag concept removed in PR #145; refs replaced tags)
- Date: 2026-09-18

## Context

Content-addressed commit hashes are stable but unreadable in runbooks;
aliases must survive shells, paths, and JSON without quoting and never
collide with reference syntax.

## Decision

`TagName` allows ASCII `[A-Za-z0-9._-]` up to 64 bytes, never all
digits. `rollback`, `export`, `diff`, and `prune --before` accept a full
64-character commit hash or a ref name; payloads carry the resolved ref/name or hash.
Duplicate names fail unless `--force` moves
the tag; tags vanish with their snapshot on prune (cascade). Bare
`tag` lists, `tag -d` deletes.

## Consequences

Automation can pin `@stable` instead of copying hashes. Renames are
explicit moves, never silent overwrites.
