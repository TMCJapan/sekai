## What changed

<!-- One paragraph: behavior change, not file list. -->

## Affected crates

<!-- e.g. `storage`, `mca`. Mark `core` explicitly when touched. -->

- [ ] `util`, `core`, `anvil`, `nbt` remain `no_std`
- [ ] No storage schema change (or: `user_version` bumped, see below)

## Safety invariants

<!-- See CONTRIBUTING.md "Safety invariants". Confirm they hold, or explain the exception. -->

- [ ] Invariants hold (or: exception explained below)

## Verification

<!-- Paste or link: fmt, clippy (incl. thumbv7m/wasm when util/core/nbt touched), tests.
      For backup-path changes, paste `backup --timing` before/after (release build). -->

```text
typos
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo check -p sekai-util -p sekai-core -p sekai-anvil -p sekai-nbt --target thumbv7m-none-eabi --no-default-features
cargo check -p sekai-util -p sekai-core -p sekai-anvil -p sekai-nbt --target wasm32-unknown-unknown --no-default-features
cargo test --workspace
cargo deny check
cargo machete
```

## Schema change (when applicable)

<!-- Old version rejected or migrated? How was `region_state`-style derived
     state handled? Which test pins the version gate? -->
