# Clone

```sh
sekai clone 1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b ./restored
```

Rebuilds a commit into a fresh directory. Unlike checkout, the live
world is never touched — clone shares the checkout restore set but
writes every file under the output directory at its layout-derived path.
The commit argument accepts a 64-character commit hash or `@tag` (see [Tags](tags.md)).

- The output directory is created when missing and must otherwise be
  empty; anything else fails loudly so clone can never clobber data.
- `--flavor legacy|new|bukkit` selects the output layout (`legacy`:
  `region/`, `DIM-1/`, `DIM1/`; `new`: `dimensions/minecraft/<name>/`;
  `bukkit`: split folders). `--base` names the overworld folder for the
  bukkit flavor (`level-name`, default `world`).
- Scope flags (`--in`, `--region`, `--kind`) restrict what is cloned;
  tombstoned regions produce no files. See
  [Scope selection](../scope.md).
- `--on-missing-blob skip-chunk` skips chunks whose blob is missing
  (default `abort`, as in checkout).
- Only vanilla namespaces are derivable; custom dimensions fail loudly
  (`UnknownRegionPath`) instead of landing somewhere wrong.
