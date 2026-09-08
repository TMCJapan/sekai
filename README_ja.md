# Sekai

*Read this in [English](README.md).*

ワールドの現在の状態をスナップショットとして保存し、任意の時点へ素早くロールバックします。

Minecraft Java 版のリージョンファイル (`.mca`) を対象とした、チャンク単位で重複排除を行うバックアップツールです。
スナップショット間で同一のチャンクデータはコンテンツアドレスストレージによって共有され、
ロールバックではスナップショットのキャプチャされたペイロードからリージョンファイルを
完全に同一かつアトミックに再構築します（可変タグである `LastUpdate` などはキャプチャ時の値に巻き戻されます）。

## インストール

このプロジェクトのビルドには Rust ツールチェインが必要です。
本リポジトリをクローンし、次のコマンドを実行してください:

```sh
cargo install --path ./crates/cli
```

## 使い方

```sh
# 現在のワールドの状態を記録する (事前にサーバーの自動セーブを停止しておくこと:
# `save-off`、`save-all` を実行し、完了後に `save-on` を実行する)
sekai --store ./sekai-store backup ./world

# スナップショットの一覧を表示する
sekai --store ./sekai-store list

# スナップショット 1 からワールドを再構築する (リージョンファイルを上書きする)
sekai --store ./sekai-store rollback ./world 1
```

`sekai` はサーバープロセスには一切干渉しません。セーブの制御は呼び出し側の責任です。
レガシー形式 (`region/`、`DIM-1/`、`DIM1/`) と新形式
(`dimensions/minecraft/...`) のいずれのワールド構成も自動的に検出されます。
設計については `ARCHITECTURE.md` を、開発上のガイドラインについては
`CONTRIBUTING.md` を参照してください。
