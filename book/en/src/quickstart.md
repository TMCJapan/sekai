# Quickstart

```sh
# Record the current world state (pause the server first; see
# "Server coordination")
sekai --store ./sekai-store commit ./world

# Preview what a commit would record, without writing anything
sekai --store ./sekai-store status ./world

# List commits
sekai --store ./sekai-store log

# Name a commit for later reference (copy its full hash from `log`)
sekai --store ./sekai-store tag create stable 2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a

# Rebuild the world from the tagged commit (overwrites region files)
sekai --store ./sekai-store checkout ./world @stable

# Compare chunk NBT between two commits (hashes from `log`)
sekai --store ./sekai-store diff 1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b 2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a --in overworld:0,0

# Preview unreferenced blobs, then collect them
sekai --store ./sekai-store gc --dry-run
sekai --store ./sekai-store gc

# Delete old commits (keep newest 10), then reclaim their blobs
sekai --store ./sekai-store prune --keep-last 10
sekai --store ./sekai-store gc

# Rebuild the tagged commit into a fresh directory (live world untouched)
sekai --store ./sekai-store clone @stable ./restored
```

`--store` names the store directory (`commit` creates it when
missing; every other command fails on a missing store). Every
command accepts `--json` for machine-readable output and, except `log`,
`tag`, and `ref`, `--timing` for a per-phase breakdown. `commit`, `status`,
`checkout`, `diff`, `clone`, `gc`, and `prune` take `--progress` for a
stderr progress bar (refused with `--json`). `commit`, `status`,
`checkout`, `diff`, and `clone` accept a scope
(`--in`, `--region`, `--kind`); nothing selected means the whole world.
