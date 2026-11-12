# ストアの場所

デフォルトでは `sekai` はカレントディレクトリの `.sekai` という
ディレクトリをストアとして使用します。この場合、すべてのコマンド
から `--store` を省略できます:

```sh
sekai commit ./world
sekai log
sekai gc
```

ストアを別の場所に置きたい場合のみ `--store` を指定します:

```sh
sekai --store ./backups/sekai commit ./world
```

`--store` はグローバルオプションで、すべてのサブコマンドで使用
できます。`commit` はストアディレクトリが存在しない場合に作成し、
他のコマンドはストアが存在しない場合に失敗します。
