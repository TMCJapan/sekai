# クイックスタート

```sh
cd ./world

# ワールドを ./world/.sekai の新しいストアに登録する（最初に1回だけ）
sekai init

# 現在のワールドの状態を記録する（事前にサーバーを一時停止してください。
# 「サーバー連携」を参照）
sekai commit

# 何も書き込まずに、コミットで記録される内容をプレビューする
sekai status

# HEAD から新しい順に履歴をたどる
sekai log --oneline

# HEAD の位置にブランチを作り、そこに切り替える
sekai branch experiment
sekai checkout experiment

# ワールドと HEAD、または2つのコミットを比較する
sekai diff --in overworld:0,0
sekai diff main experiment

# main に戻る（ワールドを main のコミットから再構築する）
sekai checkout main
```

どのコマンドも、Git がリポジトリを探すのと同じように、カレントディレクトリから見つけたワールドで動作します。`--world` と `--store` については[ストアの場所](store.md)を参照してください。すべてのコマンドは機械可読な出力を得るための `--json` に対応しています。`init`、`branch`、`tag`、`log`、`reflog`、`worktree`、`scope`、`cache clean` 以外のコマンドは、フェーズごとの内訳を表示する `--timing` にも対応しています。進捗は標準エラー出力が端末のときに表示され、`--no-progress` で止められます。`commit`、`status`、`checkout`、`diff` ではスコープ（`--in`、`--region`、`--kind`）を指定でき、何も指定しない場合はワールド全体が対象になります。
