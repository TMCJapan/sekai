# Quickstart

```sh
# Record the current world state (pause the server first; see
# "Server coordination")
sekai commit ./world

# Preview what a commit would record, without writing anything
sekai status ./world

# List commits
sekai log



# Rebuild the world from a known commit (overwrites region files)
sekai checkout ./world 2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a

# Compare chunk NBT between two commits (hashes from `log`)
sekai diff 1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b 2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a --in overworld:0,0

# Preview unreferenced blobs, then collect them
sekai gc --dry-run
sekai gc

# Delete old commits (keep newest 10), then reclaim their blobs
sekai prune --keep-last 10
sekai gc

# Rebuild a commit into a fresh directory (live world untouched)
sekai clone 2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a ./restored
```

The store defaults to `.sekai`; see [Store location](store.md) for
using another location. Every
command accepts `--json` for machine-readable output and, except `log`,
and `ref`, `--timing` for a per-phase breakdown. `commit`, `status`,
`checkout`, `diff`, `clone`, `gc`, and `prune` take `--progress` for a
stderr progress bar (refused with `--json`). `commit`, `status`,
`checkout`, `diff`, and `clone` accept a scope
(`--in`, `--region`, `--kind`); nothing selected means the whole world.
