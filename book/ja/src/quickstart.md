# クイックスタート

```sh
# 現在のワールド状態を記録（事前にサーバーの書き込みを停止してください。「サーバー連携」を参照）
sekai --store ./sekai-store backup ./world

# ワールドに変更を加えずに、バックアップで何が記録されるかをプレビュー
sekai --store ./sekai-store status ./world

# スナップショットの一覧を表示
sekai --store ./sekai-store list

# スナップショット 1 からワールドを再構築（リージョンファイルを上書き）
sekai --store ./sekai-store rollback ./world 1

# スナップショット 1 と 2 の間でチャンクの NBT を比較
sekai --store ./sekai-store diff 1 2 --in overworld:0,0

# スナップショット 2 に後から参照できるタグ名を付与
sekai --store ./sekai-store tag create stable 2

# 参照されていない Blob をプレビュー後、ガベージコレクション (GC) で回収
sekai --store ./sekai-store gc --dry-run
sekai --store ./sekai-store gc

# 古いスナップショットを削除（最新 10 件を残す）し、不要になった Blob を回収
sekai --store ./sekai-store prune --keep-last 10
sekai --store ./sekai-store gc

# スナップショット 1 を新しいディレクトリに復元（稼働中のワールドには影響しません）
sekai --store ./sekai-store export 1 ./restored

# データを変更せずにリージョンファイルを検証・検査
sekai debug scan ./world
```

`--store` にはバックアップストアのディレクトリパスを指定します（`backup` のみが存在しない場合に自動作成し、他のコマンドはエラーになります）。

すべてのコマンドは機械可読な出力を得るための `--json` に対応しています。また、`list` と `tag` を除くすべてのコマンドは、フェーズごとの処理時間内訳を表示する `--timing` に対応しています。

`backup`、`status`、`rollback`、`diff`、`export`、`gc`、`prune` では、標準エラー出力に進捗バーを表示する `--progress` を利用できます（`--json` との併用は不可）。

`backup`、`status`、`rollback`、`diff`、`export` はスコープ（`--in`, `--region`, `--kind`）による対象範囲の絞り込みに対応しています。スコープを指定しない場合はワールド全体が対象となります。
