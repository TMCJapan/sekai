# インストール

各 GitHub Release にプリビルトバイナリ（`sekai-<target triple>.tar.gz`）を添付しています。
Rust ツールチェーンがある場合（固定バージョンは `rust-toolchain.toml`）は以下でも導入できます：

```sh
cargo install --git https://github.com/TMCJapan/sekai
```

インストールされるバイナリ名は `sekai` です。

セットアップの確認：

```sh
sekai --help
```
