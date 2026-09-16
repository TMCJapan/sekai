# ADR-0004: CAS concurrent-put loser dedup

- Status: Accepted
- Date: 2026-09-10

## Context

Parallel ingest workers can `put_blob` the same hash simultaneously
(identical chunks across regions). Both writers passed the existence
check, then raced `rename(tmp, dest)`: benign overwrite on Linux,
`PermissionDenied` failure on Windows, where rename-over-existing
fails. Flaky Windows backup failures resulted.

## Decision

After a failed rename, re-check `dest`: if it exists, the winner's
bytes are identical (content-addressed), so report deduplicated
(`Ok(false)`) and clean up the temp file; otherwise propagate the
error. `new_blobs` may overcount racing duplicates on platforms where
every rename succeeds — cosmetic only, documented on `put_blob`.

## Consequences

Concurrent same-blob puts never error on any platform. Regression test
hammers one blob from 8 threads (`cas::tests`).
