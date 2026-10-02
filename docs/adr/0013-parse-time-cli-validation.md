# ADR-0013: Parse-time CLI validation and tag subcommands

- Status: Accepted
- Date: 2026-10-02

## Context

Argument validation was split between clap attributes and hand-written
checks inside command `run` functions (`tag`'s `parse_action`, `diff`'s
"internal error" fallbacks). Checks that ran after `SekaiInstance::open`
let malformed invocations create and initialize the store as a side
effect (issue #80). Clap cannot express every rule on a flat argument
list: "both `<name>` and `<snapshot>`, or neither" has no
`requires`/`ArgGroup` encoding.

## Decision

Everything statically decidable is validated by the clap structure
alone, before any command code runs:

- Operations whose operands depend on the operation become
  subcommands: `tag create <name> <snapshot> [--force]`,
  `tag delete <name>`, `tag list`. Invalid shapes are unrepresentable
  in the parsed type, so `parse_action` and its `Action` enum are gone.
  This supersedes the flat `tag` surface of ADR-0009.
- Snapshot references (`rollback`, `export`, `diff`, `prune --before`,
  `tag create`) use a `SnapshotRef` value type whose `FromStr` enforces
  the `<id>` / `@tag` grammar (mirroring `core`'s `resolve_snapshot_ref`)
  at parse time.
- Mode ambiguity becomes structure: `diff` rejects a second positional
  next to `--world` (`conflicts_with`) instead of silently ignoring it,
  and `DiffArgs::target()` projects the parsed arguments onto a total
  `DiffTarget` enum, removing the runtime "snapshot pair missing"
  fallbacks.
- Validation whose truth depends on store state stays at runtime by
  design: reference *resolution* (does the snapshot/tag exist), the
  two-snapshot minimum for a bare `diff`, and `unknown tag` on delete.

## Consequences

Malformed invocations exit with a clap usage error (exit code 2) and
never touch the filesystem; error text for those cases moves from
`anyhow` messages to clap's. The `tag` surface is a breaking CLI change,
acceptable pre-release; JSON payloads and envelope command names
(`"tag"`) are unchanged. Rules clap cannot express conditionally on a
*value* (`export --base` only meaningfully pairs with `--flavor
bukkit`) remain documented soft behavior, out of scope here.
