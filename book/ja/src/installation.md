# インストール

ソースからビルドするには Rust ツールチェーンが必要です（固定バージョンは `rust-toolchain.toml` に記載されています）。
リポジトリをクローンして以下を実行してください：

```sh
cargo install --path ./crates/cli
```

インストールされるバイナリ名は `sekai` です。
サポートされているすべてのプラットフォーム向けのリリースビルドも、CIアーティファクト（`sekai-<target triple>`）として生成されます。

セットアップの確認：

```sh
sekai --help
```
