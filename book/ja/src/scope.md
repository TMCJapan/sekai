# スコープ（対象範囲）の選択

`backup`、`rollback`、`diff`、`export` ではスコープを指定できます。
デフォルトではワールド全体が対象ですが、ディメンションごとの領域を指定することも可能です。複数の領域指定は和集合（OR条件）として結合されます。
また、`--kind`（複数指定可、省略時はすべて）は指定されたすべての領域に適用されます。

```sh
sekai --store ./sekai-store backup ./world --in overworld --kind region --kind entities
sekai --store ./sekai-store rollback ./world 3 --in overworld:0,0..31,31
sekai --store ./sekai-store diff 1 2 --in overworld:0,0
sekai --store ./sekai-store export 3 ./restored --region overworld:0,0
```

- `--in DIM`: ディメンション全体を選択します。`--in DIM:x,z` は指定した1チャンク、`--in DIM:x0,z0..x1,z1` は両端を含む矩形範囲のチャンクを選択します。
- `--region DIM:RX,RZ`: 指定したリージョンファイル内のすべてのチャンクを選択します（チャンク矩形指定の短縮記法）。
- `--kind`: リージョンの種別（`region`, `entities`, `poi`）を選択します。他のスコープ指定なしで単独で指定した場合、ワールド全体の中からその種別のみに対象を絞り込みます。

スコープは「フィルター」として機能し、データを独立して「分割（パーティショニング）」するものではありません。スコープを指定したバックアップでは、指定範囲内のみが最新の確定状態として記録されます（スコープ外の座標はレコードが作成されず、以前のスナップショットからのフォールバックによって解決されます）。同様に、スコープを指定したロールバックやエクスポートが、スコープ外のファイルに変更を加えることは決してありません。詳細については [ADR-0003](https://github.com/tmcjapan/sekai/blob/main/docs/adr/0003-scope-as-filter.md) を参照してください。
