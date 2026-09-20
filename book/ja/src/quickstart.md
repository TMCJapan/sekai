# クイックスタート

```sh
# 現在のワールド状態を記録する（事前にサーバーへの書き込みを停止してください。「サーバー連携」を参照）
sekai --store ./sekai-store backup ./world

# 現在のワールドでバックアップを取ると何が記録されるかをプレビュー
sekai --store ./sekai-store status ./world

# スナップショットの一覧表示
sekai --store ./sekai-store list

# スナップショット1からワールドを再構築（リージョンファイルを上書きします）
sekai --store ./sekai-store rollback ./world 1

# スナップショット1と2の間でチャンクNBTを比較
sekai --store ./sekai-store diff 1 2 --in overworld:0,0

# スナップショット2に後から参照できる名前を付ける
sekai --store ./sekai-store tag stable 2

# 参照されていないBlobをプレビューしてから回収
sekai --store ./sekai-store gc --dry-run
sekai --store ./sekai-store gc

# 古いスナップショットを削除（最新10件を残す）し、blob を回収
sekai --store ./sekai-store prune --keep-last 10
sekai --store ./sekai-store gc

# スナップショット1を新しいディレクトリに再構築（稼働中のワールドには影響しません）
sekai --store ./sekai-store export 1 ./restored

# 変更を加えずにリージョンファイルを検証
sekai debug scan ./world
```

`--store` はバックアップストアのディレクトリ名を指定します（存在しない場合は作成されます）。
すべてのコマンドはマシン可読な出力のための `--json` を受け付け、`list` と `tag` を除き、フェーズごとの内訳を表示する `--timing` を受け付けます。
`backup`、`status`、`rollback`、`diff`、`export`、`gc`、`prune` は標準エラー出力に進捗バーを表示する `--progress` を受け付けます（`--json` との併用は拒否されます）。
`backup`、`status`、`rollback`、`diff`、`export` は対象範囲（`--in`, `--region`, `--kind`）を受け付けます。
何も指定しない場合はワールド全体が対象となります。
