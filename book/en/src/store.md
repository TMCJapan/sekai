# Store location

By default `sekai` uses a store directory named `.sekai` in the
current working directory. In that case `--store` can be omitted
from every command:

```sh
sekai commit ./world
sekai log
sekai gc
```

Pass `--store` only when you want a store somewhere else, or a
non-default backend:

```sh
sekai --store ./backups/sekai commit ./world
sekai --store surrealkv:///var/lib/sekai commit ./world
```

`--store` is a global option and works with all subcommands.
`commit` creates the store directory when it is missing; every other
command fails on a missing store. The default backend is SurrealDB over
SurrealKV; prefix the value with `surrealkv://` (or `surrealdb://`) to
name it explicitly. A build that includes the SQLite backend selects it
with `sqlite://`.

Stores written by builds that used SurrealDB 3.x are refused by
SurrealDB 4 by default. Back up the directory, then either recreate the
store or opt in to the one-way, experimental in-place upgrade with
`SURREAL_EXPERIMENTAL_UPGRADE_FROM_3X=true`.
