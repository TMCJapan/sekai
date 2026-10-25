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
sekai --store sqlite:///var/lib/sekai commit ./world
```

`--store` is a global option and works with all subcommands.
`commit` creates the store directory when it is missing; every other
command fails on a missing store. Prefix the value with `sqlite://`
to select the SQLite backend explicitly.
