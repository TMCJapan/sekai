# 対象範囲の選択

`backup`、`rollback`、`diff`、`export` は対象範囲を受け付けます。
デフォルトではワールド全体、またはディメンションごとの領域を指定できます。
指定項目は和集合として合成されます。
`--kind`（複数指定可、空の場合はすべて）はすべての領域に適用されます。

```sh
sekai --store ./sekai-store backup ./world --in overworld --kind region --kind entities
sekai --store ./sekai-store rollback ./world 3 --in overworld:0,0..31,31
sekai --store ./sekai-store diff 1 2 --in overworld:0,0
sekai --store ./sekai-store export 3 ./restored --region overworld:0,0
```

- `--in DIM` はディメンション全体を選択します。`--in DIM:x,z` は1つのチャンク、`--in DIM:x0,z0..x1,z1` は境界を含むチャンクの矩形範囲を選択します。
- `--region DIM:RX,RZ` は1つのリージョンファイルに含まれるすべてのチャンクを選択します（矩形範囲の簡略表記）。
- `--kind` はリージョンの種類を選択します（`region`, `entities`, `poi`）。

対象範囲はフィルターであり、パーティション（分割）ではありません。
対象範囲付きのバックアップはその対象範囲内のみの事実として読み取られ（対象範囲外の座標は何も記録されず、フォールバックを通じて解決されます）、対象範囲付きのロールバックやエクスポートが対象範囲外のファイルに触れることはありません。
[ADR-0003](../../docs/adr/0003-scope-as-filter.md) を参照してください。
