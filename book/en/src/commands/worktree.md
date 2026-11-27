# Worktrees

A worktree is one world a store backs up. Each is keyed by the store
location it opens — the store root itself for the store's own (main)
worktree, a store link file for a linked one — and carries its own HEAD
(a branch name or a detached commit). Every worktree of a store shares
that store's history, blobs, and cache.

```sh
sekai worktree list
sekai worktree remove ./world-2/.sekai
```

Worktrees are a local-store feature
([ADR-0038](https://github.com/tmcjapan/sekai/blob/main/docs/adr/0038-worktrees-local-only.md)):
the backend-neutral layers know nothing of them, so these commands open
the store directly instead of going through an app operation. The store
must already exist; run `sekai init` first (see
[Store location](../store.md)).

## Listing worktrees

`worktree list` prints every worktree the store backs up, mirroring
`git worktree list`: the store's own (main) worktree first, then the
linked ones in path order, each with its abbreviated HEAD and
`[branch]` or `(detached HEAD)`:

```text
/home/me/world/.sekai    3f2a9c1 [main]
/home/me/world-2/.sekai  be71d40 [feature/village]
```

A HEAD on a branch with no commits yet is unborn and shows zeros, like
Git's null object ID.

- `--porcelain` switches to Git's record-block format: one block per
  worktree with `worktree`, `HEAD` (the full 64-hex commit ID), then
  `branch <refname>` or `detached`, blocks separated by a blank line.
  `-z` requires `--porcelain` and terminates every line, the separators
  included, with NUL instead of newline.
- `-v`/`--verbose` is accepted for compatibility but has no effect:
  sekai worktrees carry no locked or prunable annotations, and there is
  no `worktree prune`.
- `--json` reports the same rows as `path`, `commit` (`null` while
  unborn), `branch` (full refname, `null` when detached), and `main`;
  the payload contract is
  [docs/json.md](https://github.com/tmcjapan/sekai/blob/main/docs/json.md).
  It is exclusive with `--porcelain`/`-z`.

## Removing a worktree

`worktree remove <worktree>` removes a linked worktree, mirroring
`git worktree remove [-f] <worktree>`:

```sh
sekai worktree remove ./world-2/.sekai
```

The positional is the store location the worktree opens (its store link
file), the same spelling `--store` accepts for it. Removal deletes that
link and the registration only — the world directory the worktree backs
up is left untouched.

- The store's own (main) worktree is refused
  (`'<path>' is a main worktree`), as is an unregistered location
  (`'<path>' is not a worktree`). Both exit 1.
- `-f`/`--force` is accepted and repeatable (`-f -f`), but inert: sekai
  worktrees carry no dirty or locked state, and the refusals are never
  overridden.
- A link that is already gone is not an error: the registration is
  still cleaned up, like Git cleaning up a worktree whose directory
  vanished.
- `--json` returns the removed location as `path`; human output prints
  `removed worktree <path>`.

Only `list` and `remove` are exposed by the CLI. A linked worktree is
registered through the local backend API (`LocalStore::add_worktree`),
which writes the store link naming the store's absolute root; opening
that link with `--store` resolves back to the shared store.