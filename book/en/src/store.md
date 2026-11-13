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

## Secondary backups

For a full secondary backup of everything — commits, metadata, and
blobs — copy or archive the entire store directory to an external
location. Store writes are crash-ordered (CAS blobs flushed before the
metadata commit that references them), so a copied store is
self-consistent.
