# ADR-0011: A backup run marker keeps `gc` out of the write window

- Status: Accepted
- Date: 2026-09-26

## Context

A backup puts blobs into the CAS and fsyncs them *before* it commits the
`chunk_history` rows that reference them - that order is the crash-consistency
contract. `gc` decides what is unreferenced by reading metadata and then
unlinking, so a `gc` whose scan lands between those two steps sees a
backup's brand-new blobs as orphans. It unlinks them, the backup commits
rows pointing at nothing, and every later rollback, export, or diff of that
snapshot fails with `blob missing from CAS`, permanently.

The window is short (a metadata commit) but it is a real data-loss path, and
it widens with slow disks, a large ingest, or a `gc plan` reviewed minutes
after it was made.

## Decision

Mutual exclusion, not a heuristic. A marker file `<store>/backup.inflight`
is claimed for the duration of a run that writes blobs and released when the
`RunGuard` drops (so an early return or a panic still releases it). `gc`
refuses at both plan and apply time while the marker is present, and says
which file to remove.

The marker is one-directional on purpose. A crashed run leaves it behind, and
that must not be able to wedge the command operators run on a timer, so
`begin_run` refreshes an existing marker instead of refusing: only `gc` is
blocked, and only until an operator deletes one file.

Database-level locking was rejected. Holding a write transaction across a
whole backup would serialise concurrent backups and still would not help,
because the blobs are already in the CAS before the lock is taken; and a
`BEGIN IMMEDIATE` held only by `gc` does not stop a backup that has already
written its blobs.

## Consequences

`gc` and a backup can no longer overlap; the failure is loud and names the
marker instead of silently unreferencing live blobs. Two concurrent backups
on one store are still unsupported (the second refreshes the marker, so the
first may release it early) - they were never safe to begin with, since they
race on snapshot IDs. `gc` is unaffected by readers: `rollback`, `export`,
`diff`, and `list` never take the marker because they do not write blobs.
