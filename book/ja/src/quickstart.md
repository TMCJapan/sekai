# クイックスタート

```sh
# 現在のワールド状態を記録（事前にサーバーの書き込みを停止してください。「サーバー連携」を参照）
sekai --store ./sekai-store commit ./world

# ワールドに変更を加えずに、コミットで何が記録されるかをプレビュー
sekai --store ./sekai-store status ./world

# コミットの一覧を表示
sekai --store ./sekai-store log

# コミットに後から参照できるタグ名を付与（`log` で完全なハッシュを確認）
sekai --store ./sekai-store tag create stable 2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a

# タグ付けしたコミットからワールドを再構築（リージョンファイルを上書き）
sekai --store ./sekai-store checkout ./world @stable

# 2 つのコミット間でチャンクの NBT を比較（ハッシュは `log` で確認）
sekai --store ./sekai-store diff 1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b 2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a --in overworld:0,0

# 参照されていない Blob をプレビュー後、ガベージコレクション (GC) で回収
sekai --store ./sekai-store gc --dry-run
sekai --store ./sekai-store gc

# 古いコミットを削除（最新 10 件を残す）し、不要になった Blob を回収
sekai --store ./sekai-store prune --keep-last 10
sekai --store ./sekai-store gc

# タグ付けしたコミットを新しいディレクトリに復元（稼働中のワールドには影響しません）
sekai --store ./sekai-store clone @stable ./restored
```

`--store` にはストアのディレクトリパスを指定します（`commit` のみが存在しない場合に自動作成し、他のコマンドはエラーになります）。

すべてのコマンドは機械可読な出力を得るための `--json` に対応しています。また、`log` と `tag` を除くすべてのコマンドは、フェーズごとの処理時間内訳を表示する `--timing` に対応しています。

`commit`、`status`、`checkout`、`diff`、`clone`、`gc`、`prune` では、標準エラー出力に進捗バーを表示する `--progress` を利用できます（`--json` との併用は不可）。

`commit`、`status`、`checkout`、`diff`、`clone` はスコープ（`--in`, `--region`, `--kind`）による対象範囲の絞り込みに対応しています。スコープを指定しない場合はワールド全体が対象となります。
