# タグ

```sh
sekai --store ./sekai-store tag create stable 1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b
sekai --store ./sekai-store tag create stable @prev --force
sekai --store ./sekai-store tag delete stable
sekai --store ./sekai-store tag list
```

タグはコミットにわかりやすいエイリアス（別名）を付与し、チェックアウト時などの対象指定を可読性の高い状態に保ちます（64 文字のハッシュの代わりに `checkout ./world @stable` と指定可能）。
タグ名には `[A-Za-z0-9._-]` が使用でき、長さは 1〜64 バイトです。数字のみの名前は許可されません。

- `checkout`、`clone`、`diff` のコミット指定引数には、64 文字の小文字 16 進コミットハッシュまたは `@tag` を使用できます。各種レポート出力（JSON 等）には常に名前解決されたハッシュが含まれます。64 文字の 16 進表記はまずコミットハッシュとして解決されるため、同名の ref があっても隠蔽されます。形式が不正な参照は引数のパース段階で拒否されるため、ストアに触れることはありません。
- `tag create` は既存のタグ名で上書き作成しようとすると失敗します。参照先を移動させたい場合は `--force` を指定してください。また、`tag delete` で存在しないタグを削除しようとした場合はエラーになります。
- `sekai tag list` を実行すると全タグが名前順で一覧表示されます。また、`log` コマンドでも各コミットに付与されたタグが併記されます。
- タグは単なるメタデータであり、コミットの保持を強制・拘束するものではありません。タグ付きのコミットを `prune` で削除すると、付与されていたタグも一緒に削除されます。
