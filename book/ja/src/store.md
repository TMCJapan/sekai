# ストアの場所

デフォルトでは `sekai` はカレントディレクトリの `.sekai` という
ディレクトリをストアとして使用します。この場合、すべてのコマンド
から `--store` を省略できます:

```sh
sekai commit ./world
sekai log
sekai gc
```

ストアを別の場所に置きたい場合や、デフォルトではないバックエンドを
使いたい場合のみ `--store` を指定します:

```sh
sekai --store ./backups/sekai commit ./world
sekai --store surrealkv:///var/lib/sekai commit ./world
```

`--store` はグローバルオプションで、すべてのサブコマンドで使用
できます。`commit` はストアディレクトリが存在しない場合に作成し、
他のコマンドはストアが存在しない場合に失敗します。デフォルトの
バックエンドは SurrealDB (SurrealKV) です。明示的に指定する場合は
`surrealkv://` (または `surrealdb://`) を付けます。SQLite バックエンドを
含めてビルドした場合は `sqlite://` で選択できます。

SurrealDB 3.x を使ったビルドで作成したストアは、SurrealDB 4 では
デフォルトで拒否されます。ディレクトリをバックアップしたうえで、
ストアを作り直すか、`SURREAL_EXPERIMENTAL_UPGRADE_FROM_3X=true` を
指定して一方向の実験的なインプレース移行を選んでください。
