# Quickstart

```sh
cd ./world

# Register the world in a new store at ./world/.sekai (once)
sekai init

# Record the current world state (pause the server first; see
# "Server coordination")
sekai commit

# Preview what a commit would record, without writing anything
sekai status

# Walk the history from HEAD, newest first
sekai log --oneline

# Start a branch at HEAD and switch to it
sekai branch experiment
sekai checkout experiment

# Compare the world with HEAD, or two commits
sekai diff --in overworld:0,0
sekai diff main experiment

# Go back to main, rebuilding the world from its commit
sekai checkout main
```

Every command runs in the world it finds from the current directory,
as Git finds its repository; see [Store location](store.md) for
`--world` and `--store`. Every command accepts `--json` for
machine-readable output and, except `init`, `branch`, `tag`, `log`,
`reflog`, `worktree`, `scope`, and `cache clean`, `--timing` for a
per-phase breakdown. Progress shows on stderr when it is a terminal;
`--no-progress` turns it off. `commit`, `status`, `checkout`, and
`diff` accept a scope (`--in`, `--region`, `--kind`); nothing selected
means the whole world.
