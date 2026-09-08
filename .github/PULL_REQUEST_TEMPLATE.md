## What changed

<!-- One paragraph: behavior change, not file list. -->

## Affected crates

<!-- e.g. `storage`, `mca`. Mark `core` explicitly when touched. -->

- [ ] `core` is untouched (or: why the change belongs in `core`)
- [ ] No storage schema change (or: `user_version` bumped, see below)

## Safety invariants

<!-- See CONTRIBUTING.md "Safety invariants". Confirm they hold, or explain the exception. -->

- [ ] Invariants hold (or: exception explained below)

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
