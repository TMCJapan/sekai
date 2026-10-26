# Refs

```sh
sekai ref list
sekai ref create feature
sekai ref activate feature
sekai ref delete feature
```

Refs are named, movable pointers to commits - sekai's branches. A ref
points at a commit and moves forward when a new commit lands on it; a
ref without a commit is *unborn* (it names a history that has no
commit yet).

Exactly one ref is **active**: the one new commits hang from. A fresh
store starts with one active ref, `main`, unborn. `ref list` marks it
with `*`.

- `ref create <name>` starts the ref at the active ref's target; pass a
  commit (`<id>` or another ref name) to point it elsewhere.
  `ref create` fails over an existing name unless `--force` moves it.
- `ref activate <name>` switches the active ref. The next commit
  continues that ref instead of the newest commit in the store, so
  switching forks the history the way `git switch` does.
- `ref delete <name>` removes a ref. The active ref is never removed -
  a store without one would have nowhere to hang new commits from.
- Ref names allow ASCII alphanumerics plus `.`, `_`, `-`, and `/`, run
  1-64 bytes, and are never all digits, so a name cannot be confused
  with a commit ID. `HEAD` is reserved for the store's active ref.

Refs are metadata only and constrain nothing: pruning a commit hands
its ref to the commit that folded it.
