# タグ

```sh
sekai --store ./sekai-store tag create stable 1
sekai --store ./sekai-store tag create stable @prev --force
sekai --store ./sekai-store tag delete stable
sekai --store ./sekai-store tag list
```

タグはスナップショットにわかりやすいエイリアス（別名）を付与し、ロールバック時などの対象指定を可読性の高い状態に保ちます（数値 ID の代わりに `rollback ./world @stable` と指定可能）。
タグ名には `[A-Za-z0-9._-]` が使用でき、長さは 1〜64 バイトです。数字のみの名前は許可されないため、`@123` がスナップショット ID `123` と誤認されることはありません。

- `rollback`、`export`、`diff` のスナップショット指定引数には、`<id>` または `@tag` を使用できます。なお、各種レポート出力（JSON 等）には常に名前解決された数値 ID が含まれます。形式が不正な参照は引数のパース段階で拒否されるため、ストアに触れることはありません。
- `tag create` は既存のタグ名で上書き作成しようとすると失敗します。参照先を移動させたい場合は `--force` を指定してください。また、`tag delete` で存在しないタグを削除しようとした場合はエラーになります。
- `sekai tag list` を実行すると全タグが名前順で一覧表示されます。また、`list` コマンドでも各スナップショットに付与されたタグが併記されます。
- タグは単なるメタデータであり、スナップショットの保持を強制・拘束するものではありません。タグ付きのスナップショットを `prune` で削除すると、付与されていたタグも一緒に削除されます。
