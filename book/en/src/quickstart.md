# Quickstart

```sh
# Record the current world state (pause the server first; see
# "Server coordination")
sekai --store ./sekai-store commit ./world

# Preview what a commit would record, without writing anything
sekai --store ./sekai-store status ./world

# List commits
sekai --store ./sekai-store log

# Rebuild the world from commit 1 (overwrites region files)
sekai --store ./sekai-store checkout ./world 1

# Compare chunk NBT between commits 1 and 2
sekai --store ./sekai-store diff 1 2 --in overworld:0,0

# Name commit 2 for later reference
sekai --store ./sekai-store tag create stable 2

# Preview unreferenced blobs, then collect them
sekai --store ./sekai-store gc --dry-run
sekai --store ./sekai-store gc

# Delete old commits (keep newest 10), then reclaim their blobs
sekai --store ./sekai-store prune --keep-last 10
sekai --store ./sekai-store gc

# Rebuild commit 1 into a fresh directory (live world untouched)
sekai --store ./sekai-store clone 1 ./restored
```

`--store` names the store directory (`commit` creates it when
missing; every other command fails on a missing store). Every
command accepts `--json` for machine-readable output and, except `log`
and `tag`, `--timing` for a per-phase breakdown. `commit`, `status`,
`checkout`, `diff`, `clone`, `gc`, and `prune` take `--progress` for a
stderr progress bar (refused with `--json`). `commit`, `status`,
`checkout`, `diff`, and `clone` accept a scope
(`--in`, `--region`, `--kind`); nothing selected means the whole world.
