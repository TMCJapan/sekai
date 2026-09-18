# ADR-0008: Prune folds into the next retained snapshot, `gc` reclaims blobs

- Status: Accepted
- Date: 2026-09-18

## Context

Snapshot deletion must never change what retained snapshots restore,
and blob unlinking must keep the crash order (metadata commit before
unlink). Doing both in one step would tangle row restamping with CAS
durability.

## Decision

Pruning is oldest-first folding: each retired snapshot drops rows
already superseded at or before the next retained snapshot and
re-stamps surviving rows onto it; derived `region_state` follows, the
snapshot row (plus its tags, by cascade) is deleted — all atomically.
Pruning only dereferences blobs; a later `gc` reclaims them. The CLI
requires at least one selector (`--keep-last` / `--before`), refuses a
selection that retains nothing, and `--dry-run` lists IDs only.

## Consequences

Retained snapshots restore byte-identically before and after pruning.
Operators must run `gc` after `prune`; the CLI says so on completion.
