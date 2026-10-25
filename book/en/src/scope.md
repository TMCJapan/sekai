# Scope selection

`commit`, `checkout`, `diff`, and `clone` accept a scope: the whole
world by default, or per-dimension areas. Entries compose by union;
`--kind` (repeatable, empty means all) applies to every area.

```sh
sekai commit ./world --in overworld --kind region --kind entities
sekai checkout ./world 3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d --in overworld:0,0..31,31
sekai diff 1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b 2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a --in overworld:0,0
sekai clone 3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d ./restored --region overworld:0,0
```

- `--in DIM` selects a whole dimension; `--in DIM:x,z` one chunk;
  `--in DIM:x0,z0..x1,z1` an inclusive chunk rectangle.
- `--region DIM:RX,RZ` selects every chunk of one region file
  (rectangle shorthand).
- `--kind` selects region families (`region`, `entities`, `poi`). Alone it
  narrows the whole world to those families.

The scope is a filter, not a partition: a scoped commit reads as the
truth for its scope only (out-of-scope coordinates record nothing and
resolve through fallback), and a scoped checkout or clone never touches
files outside the scope. See [ADR-0003](https://github.com/tmcjapan/sekai/blob/main/docs/adr/0003-scope-as-filter.md).
