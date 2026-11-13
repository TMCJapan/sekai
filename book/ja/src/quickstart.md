# クイックスタート

```sh
# ストアを作成（初回の一度だけ。他のコマンドより先に実行）
sekai init

# 現在のワールド状態を記録（事前にサーバーの書き込みを停止してください。「サーバー連携」を参照）
sekai commit ./world

# ワールドに変更を加えずに、コミットで何が記録されるかをプレビュー
sekai status ./world

# コミットの一覧を表示
sekai log


# タグ付けしたコミットからワールドを再構築（リージョンファイルを上書き）
sekai checkout ./world 2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a

# 2 つのコミット間でチャンクの NBT を比較（ハッシュは `log` で確認）
sekai diff 1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b 2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a --in overworld:0,0
```

ストアはデフォルトで `.sekai` を使用します（別の場所を使う場合は [ストアの場所](store.md) を参照）。

すべてのコマンドは機械可読な出力を得るための `--json` に対応しています。また、`log`、`ref`、`init` を除くすべてのコマンドは、フェーズごとの処理時間内訳を表示する `--timing` に対応しています。

`commit`、`status`、`checkout`、`diff` では、標準エラー出力に進捗バーを表示する `--progress` を利用できます（`--json` との併用は不可）。

`commit`、`status`、`checkout`、`diff` はスコープ（`--in`, `--region`, `--kind`）による対象範囲の絞り込みに対応しています。スコープを指定しない場合はワールド全体が対象となります。
