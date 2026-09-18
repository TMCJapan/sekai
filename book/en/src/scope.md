# Scope selection

`backup`, `rollback`, `diff`, and `export` accept a scope: the whole
world by default, or per-dimension areas. Entries compose by union;
`--kind` (repeatable, empty means all) applies to every area.

```sh
sekai --store ./sekai-store backup ./world --in overworld --kind region --kind entities
sekai --store ./sekai-store rollback ./world 3 --in overworld:0,0..31,31
sekai --store ./sekai-store diff 1 2 --in overworld:0,0
sekai --store ./sekai-store export 3 ./restored --region overworld:0,0
```

- `--in DIM` selects a whole dimension; `--in DIM:x,z` one chunk;
  `--in DIM:x0,z0..x1,z1` an inclusive chunk rectangle.
- `--region DIM:RX,RZ` selects every chunk of one region file
  (rectangle shorthand).
- `--kind` selects region families (`region`, `entities`, `poi`).

The scope is a filter, not a partition: a scoped backup reads as the
truth for its scope only (out-of-scope coordinates record nothing and
resolve through fallback), and a scoped rollback or export never touches
files outside the scope. See [ADR-0003](https://github.com/TMCJapan/sekai/blob/main/docs/adr/0003-scope-as-filter.md).
