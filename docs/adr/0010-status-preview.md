# ADR-0010: `status` is the backup dry-run path

- Status: Accepted
- Date: 2026-09-18

## Context

Operators need a read-only preview of what a backup would record.
A `backup --dry-run` flag would split every write path with a preview
branch and risk the two drifting apart.

## Decision

There is no `backup --dry-run`: `status` is that command. It runs the
same observe, plan, and ingest path with blob puts replaced by
existence probes and no metadata commit, then reports the staged
counts. Counts match a subsequent backup unless the world changes in
between. Diff views stay off in previews.

## Consequences

One ingest path serves both flows, so preview numbers cannot rot.
`status` never writes to the world or the store by construction.
