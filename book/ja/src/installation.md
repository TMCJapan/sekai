# インストール

各 GitHub Release にプリビルトバイナリ（`sekai-<target triple>.tar.gz`）を添付しています。
Rust ツールチェーンがある場合（固定バージョンは `rust-toolchain.toml`）は以下でも導入できます：

```sh
cargo install sekai-cli
```

インストールされるバイナリ名は `sekai` です。
現在のリポジトリのソースから自分でビルドする場合は以下です：

```sh
cargo install --path ./crates/cli
```

セットアップの確認：

```sh
sekai --help
```
