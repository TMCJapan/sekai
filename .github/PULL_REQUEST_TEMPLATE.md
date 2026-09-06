# Pull Request

## What changed

<!-- One paragraph: behavior change, not file list. -->

## Affected crates

<!-- e.g. `engine`, `storage`. Mark `core` explicitly when touched. -->

- [ ] `core` is untouched (or: why the change belongs in `core`)
- [ ] No storage schema change (or: `user_version` bumped, see below)

## Invariant checklist

<!-- From AGENTS.md. Check every box or explain the exception. -->

- [ ] `core` stays `no_std` with zero dependencies
- [ ] No in-place `.mca` mutation (same-dir temp + atomic rename)
- [ ] Byte-perfect rollback preserved (raw payloads in CAS, normalization only for views)
- [ ] No server orchestration inside libraries (CLI/caller layer only)
- [ ] Derived state stays rebuildable
- [ ] Destructive operations keep a dry-run / apply split

## Verification

<!-- Paste or link: fmt, clippy (incl. thumbv7m/wasm when core/nbt touched), tests.
     For backup-path changes, paste `backup --timing` before/after (release build). -->

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Schema change (when applicable)

<!-- Old version rejected or migrated? How was `region_state`-style derived
     state handled? Which test pins the version gate? -->
