## What changed

<!-- One paragraph: behavior change, not file list. -->

## Affected crates

<!-- e.g. `core`, `storage`, `anvil`. -->

- [ ] `util`, `core`, `anvil`, and `nbt` remain `no_std`
- [ ] No storage schema change (or: `user_version` bumped, see below)
- [ ] JSON output unchanged (or: golden tests + `docs/json.md` updated together)

## Safety invariants

<!-- See CONTRIBUTING.md "Safety invariants". Confirm they hold, or explain the exception. -->

- [ ] Invariants hold (or: exception explained below)

## Verification

<!-- Paste or link: fmt, clippy (incl. thumbv7m/wasm when util/core/nbt touched), tests.
      For perf-labeled or hot-path changes, paste the affected command's
      `--timing` before/after (release build). -->

```text
typos
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p sekai-util -p sekai-anvil -p sekai-nbt -p sekai-core --target thumbv7m-none-eabi -- -D warnings
cargo clippy -p sekai-util -p sekai-anvil -p sekai-nbt -p sekai-core --target wasm32-unknown-unknown -- -D warnings
cargo nextest run --workspace
cargo deny check
cargo machete
```

## Schema change (when applicable)

<!-- Old version rejected or migrated? How was `region_state`-style derived
     state handled? Which test pins the version gate? -->
