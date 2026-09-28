# インストール

各 [GitHub Release](https://github.com/tmcjapan/sekai/releases) にて、プリビルトバイナリ（`sekai-<target triple>.tar.gz`）を配布しています。

また、Rust ツールチェーンが導入されている環境（推奨バージョンは `rust-toolchain.toml` を参照）であれば、以下のように Cargo で直接インストールすることもできます:

```sh
cargo install --git https://github.com/tmcjapan/sekai
```

インストールされるバイナリ名は `sekai` です。

正しくインストールされたか確認するには、以下を実行してください:

```sh
sekai --help
```
