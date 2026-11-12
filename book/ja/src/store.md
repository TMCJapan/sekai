# ストアの場所

デフォルトでは `sekai` はカレントディレクトリの `.sekai` という
ディレクトリをストアとして使用します。この場合、すべてのコマンド
から `--store` を省略できます:

```sh
sekai init
sekai commit ./world
sekai log
sekai gc
```

ストアを別の場所に置きたい場合のみ `--store` を指定します:

```sh
sekai --store ./backups/sekai init
sekai --store ./backups/sekai commit ./world
```

`--store` はグローバルオプションで、すべてのサブコマンドで使用
できます。

## ストアの初期化

ストアを作成できるのは `sekai init` だけです。他のコマンドより
先に一度実行してください。それ以外のすべてのコマンド（`commit`
を含む）は、ストアが存在しない場合に失敗し、先に `sekai init` を
実行するよう促します。

`git init` と同様に、既存のストアに対して `init` を再実行しても
安全です。再初期化された旨が表示され、記録済みの履歴には一切
変更が加わりません。
