# Export

```sh
sekai --store ./sekai-store export 1 ./restored
```

Rebuilds a snapshot into a fresh directory. Unlike rollback, the live
world is never touched — export shares the rollback restore set but
writes every file under the output directory at its layout-derived path.

- The output directory is created when missing and must otherwise be
  empty; anything else fails loudly so export can never clobber data.
- `--flavor legacy|new|bukkit` selects the output layout (`legacy`:
  `region/`, `DIM-1/`, `DIM1/`; `new`: `dimensions/minecraft/<name>/`;
  `bukkit`: split folders). `--base` names the overworld folder for the
  bukkit flavor (`level-name`, default `world`).
- Scope flags restrict what is exported; tombstoned regions produce no
  files.
- `--on-missing-blob skip-chunk` skips chunks whose blob is missing
  (default `abort`, as in rollback).
- Only vanilla namespaces are derivable; custom dimensions fail loudly
  (`UnknownRegionPath`) instead of landing somewhere wrong.
