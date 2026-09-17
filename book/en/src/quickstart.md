# Quickstart

```sh
# Record the current world state (pause the server first; see
# "Server coordination")
sekai --store ./sekai-store backup ./world

# List snapshots
sekai --store ./sekai-store list

# Rebuild the world from snapshot 1 (overwrites region files)
sekai --store ./sekai-store rollback ./world 1

# Compare chunk NBT between snapshots 1 and 2
sekai --store ./sekai-store diff 1 2 --in overworld:0,0

# Preview unreferenced blobs, then collect them
sekai --store ./sekai-store gc --dry-run
sekai --store ./sekai-store gc

# Rebuild snapshot 1 into a fresh directory (live world untouched)
sekai --store ./sekai-store export 1 ./restored

# Inspect region files without touching anything
sekai debug scan ./world
```

`--store` names the backup store directory (created when missing). Every
command accepts `--json` for machine-readable output and, except `list`,
`--timing` for a per-phase breakdown. `backup`, `rollback`, `diff`,
`export`, and `gc` take `--progress` for a stderr progress bar (refused
with `--json`). `backup`, `rollback`, `diff`, and `export` accept a scope
(`--in`, `--region`, `--kind`); nothing selected means the whole world.
