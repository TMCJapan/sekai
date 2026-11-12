# Scope selection

`commit`, `checkout`, and `diff` accept a scope: the whole
world by default, or per-dimension areas. Entries compose by union;
`--kind` (repeatable, empty means all) applies to every area.

```sh
sekai commit ./world --in overworld --kind region --kind entities
sekai checkout ./world 3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d --in overworld:0,0..31,31
sekai diff 1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b 2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a --in overworld:0,0
```

- `--in DIM` selects a whole dimension; `--in DIM:x,z` one chunk;
  `--in DIM:x0,z0..x1,z1` an inclusive chunk rectangle.
- `--region DIM:RX,RZ` selects every chunk of one region file
  (rectangle shorthand).
- `--kind` selects region families (`region`, `entities`, `poi`). Alone it
  narrows the whole world to those families.

The scope is a filter, not a partition: a scoped commit reads as the
truth for its scope only (out-of-scope coordinates record nothing and
resolve through fallback), and a scoped checkout never touches
files outside the scope. See [ADR-0003](https://github.com/tmcjapan/sekai/blob/main/docs/adr/0003-scope-as-filter.md).

## Named scopes

An area you select over and over can be saved in the store once and
selected by name with `--scope` on `commit`, `status`, `checkout`,
and `diff`. Scopes live in the store, so it must exist first:
run `sekai init` before `scope create` on a new store.

```sh
sekai scope create spawn --in overworld:-8,-8..7,7 --region nether:0,0
sekai commit ./world --scope spawn
sekai scope list
sekai scope delete spawn
```

- `scope create <name>` takes one or more `--in DIM:x,z`,
  `--in DIM:x0,z0..x1,z1`, or `--region DIM:RX,RZ` areas. A whole
  dimension (`--in DIM`) is refused: every area is stored as a chunk
  rectangle. An existing name fails unless `--force` replaces it.
- A named scope covers every region family; `--scope` cannot be combined
  with `--in`, `--region`, or `--kind`, and names one scope per run.
- The name is looked up when the command runs: replacing a scope changes
  what later runs select, never what earlier commits hold. An unknown
  name fails.
- Scope names follow the ref name rules (ASCII alphanumerics plus `.`,
  `_`, `-`, `/`; 1-64 bytes), but may be all digits.

See [ADR-0021](https://github.com/tmcjapan/sekai/blob/main/docs/adr/0021-named-scopes.md).
