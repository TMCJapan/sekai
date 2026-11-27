# ワークツリー

ワークツリーとは、1つのストアがバックアップする1つのワールドのことです。
各ワークツリーは、開くストアの場所（ストア自身＝メインはストアのルート、
リンクされたものはストアリンクファイル）をキーとして登録され、それぞれ
独立した HEAD（ブランチ名またはデタッチされたコミット）を持ちます。
同じストアのワークツリーは、その履歴・Blob・キャッシュを共有します。

```sh
sekai worktree list
sekai worktree remove ./world-2/.sekai
```

ワークツリーはローカルストアの機能です
（[ADR-0038](https://github.com/tmcjapan/sekai/blob/main/docs/adr/0038-worktrees-local-only.md)）。
バックエンド非依存の層はワークツリーを関知しないため、これらのコマンドは
アプリの操作を経由せず、ストアを直接開いて処理します。ストアは事前に
作成しておく必要があります。先に `sekai init` を実行してください
（[ストアの場所](../store.md)を参照）。

## ワークツリーの一覧

`worktree list` はストアがバックアップするすべてのワークツリーを
`git worktree list` と同じ形式で表示します。ストア自身（メイン）の
ワークツリーが先頭、以降はリンクされたものがパス順で、各行に短縮された
HEAD と `[branch]` または `(detached HEAD)` が並びます:

```text
/home/me/world/.sekai    3f2a9c1 [main]
/home/me/world-2/.sekai  be71d40 [feature/village]
```

まだコミットのないブランチ上の HEAD は未出生（unborn）で、Git の
null オブジェクト ID と同様にゼロ表示になります。

- `--porcelain` は Git のレコードブロック形式に切り替えます。ワークツリー
  ごとに `worktree`、`HEAD`（64桁の完全なコミット ID）、続いて
  `branch <refname>` または `detached` を出力し、ブロック間は空行で
  区切ります。`-z` は `--porcelain` が必須で、区切りを含むすべての行を
  改行ではなく NUL で終端します。
- `-v`/`--verbose` は互換性のために受け付けますが効果はありません。
  sekai のワークツリーには locked / prunable の注釈がなく、
  `worktree prune` も存在しないためです。
- `--json` は同じ内容を `path`、`commit`（未出生なら `null`）、`branch`
  （完全な refname、デタッチ中は `null`）、`main` として出力します。
  仕様は
  [docs/json.md](https://github.com/tmcjapan/sekai/blob/main/docs/json.md)
  を参照してください。`--porcelain`/`-z` との併用はできません。

## ワークツリーの削除

`worktree remove <worktree>` はリンクされたワークツリーを削除します。
`git worktree remove [-f] <worktree>` に相当します:

```sh
sekai worktree remove ./world-2/.sekai
```

位置引数はそのワークツリーが開くストアの場所（ストアリンクファイル）で、
`--store` に指定するのと同じ表記です。削除されるのはそのストアリンクと
登録だけで、ワークツリーがバックアップするワールドディレクトリには
一切触れません。

- ストア自身（メイン）のワークツリーは拒否され
  （`'<path>' is a main worktree`）、未登録の場所も拒否されます
  （`'<path>' is not a worktree`）。どちらも終了コード 1 です。
- `-f`/`--force` は受け付けられ（`-f -f` と重ねることもできます）、
  効果はありません。sekai のワークツリーには dirty / locked の状態が
  なく、上記の拒否が上書きされることもないためです。
- リンクが既に存在しない場合もエラーにはなりません。Git がディレクトリ
  の消えたワークツリーを後始末するのと同様に、登録だけがクリーンアップ
  されます。
- `--json` は削除した場所を `path` として返します。通常テキスト出力では
  `removed worktree <path>` と表示します。

CLI が提供するのは `list` と `remove` だけです。リンクされたワークツリーの
登録はローカルバックエンドの API（`LocalStore::add_worktree`）が行い、
ストアの絶対ルートを記したストアリンクファイルを書き込みます。そのリンクを
`--store` で開くと、共有ストアに解決されます。