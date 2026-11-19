# ブランチとタグ

```sh
sekai branch                      # 一覧（* は HEAD のブランチ）
sekai branch experiment           # HEAD の位置にブランチを作る
sekai branch -f experiment v1     # ブランチを動かす
sekai branch -m trunk             # HEAD のブランチの名前を変える
sekai branch -d experiment        # HEAD から到達できるブランチを削除する
sekai tag v1                      # HEAD にタグを付ける
sekai tag -d v1
sekai reflog                      # HEAD がたどってきた位置
```

ブランチとタグは Git と同じように、Git のモードオプションで操作します。

- `branch [--list] [<pattern>...]` はブランチを一覧表示します。`*` は HEAD のブランチ、`+` は別のワークツリーでチェックアウトされているブランチを表し、HEAD が detach しているときは先頭に `(HEAD detached at <id>)`（HEAD のコミット）と表示します。
- `branch [-f] <name> [<start>]` は `<start>`（省略時は HEAD）の位置にブランチを作ります。`-f` を付けると既存のブランチを動かします。チェックアウトされているブランチは動かしません。
- `branch (-d | -D) <name>...` はブランチを削除します。`-d` は HEAD から到達できないブランチを拒否し、`-D` はそれでも削除します。チェックアウトされているブランチは削除しません。
- `branch (-m | -M) [<old>] <new>` はブランチの名前を変えます。新しい名前だけを指定すると HEAD のブランチが対象になり、`-M` は既存のブランチを上書きします。
- `branch --show-current` は HEAD のブランチを表示します。
- `tag [-f] <name> [<commit-ish>]` はコミット（省略時は HEAD）にタグを付け、`-f` でタグを置き換えます。`tag -d` はタグを削除し、`tag -l` は一覧を表示します。タグは軽量タグだけで、メッセージや署名はありません。
- `reflog [show] [<ref>]` は、HEAD またはブランチが指してきた位置を新しい順に `<id> HEAD@{<n>}: <message>` の形で表示します。

名前には ASCII 英数字と `.`、`_`、`-`、`/` が使え、長さは 1〜255 バイトです。空の要素は使えず、`..` を含めることもできません。新しい名前を `-` で始めることもできません。`HEAD` は予約されています。

## リビジョン

コミットを指定する場所では、コミット ID（64 文字すべて）、ref の名前、`HEAD` または `@`、Git と同じ接尾辞を使えます。

- `<rev>~<n>`: 第1親を n 回たどった祖先

`log` では `<a>..<b>` と `^<rev>` も、`diff` では `<a>..<b>` も使えます。
