# Store location

A world's store sits at its root as `.sekai`, as a Git repository sits
in `.git`. Run commands anywhere inside the world: sekai walks up from
the current directory to the first `.sekai`, which names the store and
the worktree.

```sh
cd ./world
sekai init
sekai commit
sekai log
```

Two global options change where sekai looks:

- `--world <dir>` starts the search at `<dir>` instead of the current
  directory.
- `--store <dir>` uses another store. With `init`, the world becomes a
  linked worktree of that store, and `init` writes a `.sekai` link file
  that names it, so later commands find the store from the world alone.

```sh
sekai --world ./world --store ./backups/sekai init
sekai --world ./world commit
```

A world belongs to one store: `init` refuses a world whose `.sekai`
names another. Outside every world, `--store` still runs the commands
that need no world, such as `log --all`, `branch`, `tag`, `scope`, and
`worktree list`.

## Initializing a store

`sekai init` is the only command that creates a store; run it once in
a world before anything else. Every other command fails on a missing
store and asks you to run `sekai init` first.

Like `git init`, re-running `init` is safe: it reports a
re-initialization and leaves the recorded history untouched. HEAD
starts unborn on `main`, so the first commit creates the branch.

## Secondary backups

For a full secondary backup of everything — commits, metadata, blobs,
and the region-image cache — copy or archive the entire store directory
to an external location. Store writes are crash-ordered (CAS blobs
flushed before the metadata commit that references them), so a copied
store is self-consistent. The [cache](commands/cache.md) is derived and
rebuildable, so leaving it out of a backup only costs performance on the
next run.
