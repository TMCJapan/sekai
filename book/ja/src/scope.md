# スコープ（対象範囲）の選択

`commit`、`checkout`、`diff` ではスコープを指定できます。
デフォルトではワールド全体が対象ですが、ディメンションごとの領域を指定することも可能です。複数の領域指定は和集合（OR条件）として結合されます。
また、`--kind`（複数指定可、省略時はすべて）は指定されたすべての領域に適用されます。

```sh
sekai commit ./world --in overworld --kind region --kind entities
sekai checkout ./world 3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d --in overworld:0,0..31,31
sekai diff 1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b 2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a --in overworld:0,0
```

- `--in DIM`: ディメンション全体を選択します。`--in DIM:x,z` は指定した1チャンク、`--in DIM:x0,z0..x1,z1` は両端を含む矩形範囲のチャンクを選択します。
- `--region DIM:RX,RZ`: 指定したリージョンファイル内のすべてのチャンクを選択します（チャンク矩形指定の短縮記法）。
- `--kind`: リージョンの種別（`region`, `entities`, `poi`）を選択します。他のスコープ指定なしで単独で指定した場合、ワールド全体の中からその種別のみに対象を絞り込みます。

スコープは「フィルター」として機能し、データを独立して「分割（パーティショニング）」するものではありません。スコープを指定したコミットでは、指定範囲内のみが最新の確定状態として記録されます（スコープ外の座標はレコードが作成されず、以前のコミットからのフォールバックによって解決されます）。同様に、スコープを指定したチェックアウトやクローンが、スコープ外のファイルに変更を加えることは決してありません。詳細については [ADR-0003](https://github.com/tmcjapan/sekai/blob/main/docs/adr/0003-scope-as-filter.md) を参照してください。

## 名前付きスコープ

繰り返し使う範囲は、一度 store に保存しておけば、`commit`、`status`、`checkout`、`diff` で `--scope` に名前を指定して選択できます。スコープは store に保存されるため、先に store が必要です。新しい store では、`scope create` の前に一度通常の `commit` を実行してください。

```sh
sekai scope create spawn --in overworld:-8,-8..7,7 --region nether:0,0
sekai commit ./world --scope spawn
sekai scope list
sekai scope delete spawn
```

- `scope create <name>` には、`--in DIM:x,z`、`--in DIM:x0,z0..x1,z1`、`--region DIM:RX,RZ` の範囲を1つ以上指定します。各範囲はチャンクの矩形として保存されるため、ディメンション全体（`--in DIM`）は指定できません。同じ名前のスコープがすでにある場合は失敗します。`--force` を付けると置き換えます。
- 名前付きスコープは常にすべてのリージョン種別を対象にします。`--scope` は `--in`、`--region`、`--kind` と同時に指定できず、1回の実行で指定できるスコープは1つです。
- 名前はコマンドの実行時に解決されます。スコープを置き換えると以後の実行で選択される範囲は変わりますが、過去のコミットの内容は変わりません。存在しない名前を指定すると失敗します。
- スコープ名の規則は ref 名と同じです（ASCII 英数字と `.`、`_`、`-`、`/`、1〜64 バイト）。ただし数字だけの名前も使えます。

詳細については [ADR-0021](https://github.com/tmcjapan/sekai/blob/main/docs/adr/0021-named-scopes.md) を参照してください。
