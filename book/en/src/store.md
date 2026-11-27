# Store location

By default `sekai` uses a store directory named `.sekai` in the
current working directory. In that case `--store` can be omitted
from every command:

```sh
sekai init
sekai commit ./world
sekai log
```

Pass `--store` only when you want a store somewhere else:

```sh
sekai --store ./backups/sekai init
sekai --store ./backups/sekai commit ./world
```

`--store` is a global option and works with all subcommands.

## Initializing a store

`sekai init` is the only command that creates a store; run it once
before anything else. Every other command (including `commit`) fails
on a missing store and asks you to run `sekai init` first.

Like `git init`, re-running `init` on an existing store is safe: it
reports a re-initialization and leaves the recorded history untouched.

## Store configuration

`sekai config` reads and writes store settings in `<store>/config.toml`,
like `git config` limited to the repository scope — there is no global
config. The first setting is the commit author name:

```sh
sekai config user.name "Alice"   # set
sekai config user.name           # read (prints just the value)
sekai config --list              # every set key as key=value
sekai config user.name --unset   # remove
```

Keys are case-insensitive; an unset key fails with exit 1 unless
`--default <value>` supplies a fallback. Like every command, `config`
opens the store exclusively, so it cannot run while another process
holds the store (a running backup); a store link resolves to the real
store root. `--json` emits the standard envelope (see `docs/json.md`).

## Worktrees

A store can back up several worlds, each registered as a worktree opened
from a store location: the store root itself for the store's own (main)
worktree, a store link file for a linked one. Any spelling of either
location opens the same worktree, and a moved or copied store keeps its
main worktree. Linked worktrees
share the store's history, blobs, and cache. See
[Worktrees](commands/worktree.md) for `sekai worktree list` and
`sekai worktree remove`.

## Secondary backups

For a full secondary backup of everything — commits, metadata, blobs,
and the region-image cache — copy or archive the entire store directory
to an external location. Store writes are crash-ordered (CAS blobs
flushed before the metadata commit that references them), so a copied
store is self-consistent. The [cache](commands/cache.md) is derived and
rebuildable, so leaving it out of a backup only costs performance on the
next run.
