# ADR-0040: Worktree remove unregisters a linked worktree, leaving the world

- Status: Accepted
- Date: 2026-11-27

## Context

`git worktree remove [-f] <worktree>` deletes a linked worktree's
directory and its administrative files, refusing a dirty or locked
worktree unless forced. Sekai worktrees (ADR-0038) are keyed by the store
location they open — a store link file for a linked one — and
`LocalStore::add_worktree` writes only that link and a registration row;
it never materializes the world directory the worktree backs up. The
world is the caller's source data, not a checkout the store created, and
worktree HEADs do not drive commits yet, so there is no per-worktree
"dirty" state to check. The JSON contract gains a removal payload.

## Decision

Add `sekai worktree remove [-f] <worktree>` over
`LocalStore::remove_worktree`, the inverse of `add_worktree`:

- The positional is the store location the worktree opens (its store link
  file), the same spelling `--store` accepts for it; it resolves through
  the same `store_key` normalization as every worktree key.
- Removal deletes the store link and the registration row only. The world
  directory is left untouched: add did not materialize it, so remove never
  deletes it. This is the deliberate deviation from Git, which deletes the
  worktree directory — a sekai world may hold the only copy of uncommitted
  changes, and the store backs it up rather than owns it.
- The store's own (main) worktree and an unregistered location are refused
  with Git's wording (`'<path>' is a main worktree`, `'<path>' is not a
  worktree`); neither refusal is ever overridden. A link that is already
  gone is not an error — the row is still removed, like Git cleaning up a
  worktree whose directory vanished.
- The link is deleted before the row so a failed delete leaves a row a
  retry can finish, never a leftover link that no later remove can reach
  (an absent row is refused as unregistered).
- `-f`/`--force` is accepted and repeatable (`-f -f`) as Git's force is,
  but inert: sekai worktrees carry no dirty or locked state to override.
  It gains Git's meaning when per-worktree world tracking lands.
- The JSON payload is a removal record `{"path": <absolute key>}`, the same
  identity `worktree list` reports; human output prints
  `removed worktree <path>`.

## Consequences

Unregistering a worktree can never lose world data, and the operation is
retry-safe. The cost is that `--force` is cosmetic until worktree HEADs
drive commits and a dirty check becomes definable; the flag is kept for
argument parity with Git and documented as inert. No schema change:
removal deletes rows from the existing `worktrees` table. `docs/json.md`
documents the payload.