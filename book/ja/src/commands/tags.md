# タグ

```sh
sekai --store ./sekai-store tag stable 1
sekai --store ./sekai-store tag stable @prev --force
sekai --store ./sekai-store tag -d stable
sekai --store ./sekai-store tag
```

タグはスナップショットに別名を与え、ロールバック対象を読みやすく保ちます（ID ではなく `rollback ./world @stable`）。
名前は `[A-Za-z0-9._-]`、1–64 バイト、全て数字の名前は弾かれるため、`@123` がスナップショット `123` と混同されることはありません。

- `rollback`・`export`・`diff` のスナップショット引数は `<id>` または `@tag` を受け付けます。レポートは常に解決後の数値 ID を運びます。
- 既存名への作成は `--force` なしでは失敗し、タグを移動させます。存在しないタグの削除は loud に失敗します。
- 引数なしの `tag` は全タグを名前順に一覧し、`list` は各スナップショットのタグを併記します。
- タグはメタデータのみで何も拘束しません。タグ付きスナップショットを prune するとタグも一緒に消えます。
