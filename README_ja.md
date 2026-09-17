# Sekai

*Read this in [English](README.md).*

ワールドの現在の状態をスナップショットとして保存し、任意の時点へ素早くロールバックします。

Minecraft Java 版のリージョンファイル (`.mca`) を対象とした、チャンク単位で重複排除を行うバックアップツールです。
スナップショット間で同一のチャンクデータはコンテンツアドレスストレージによって共有され、
ロールバックではスナップショット時にキャプチャされたデータからリージョンファイルを
完全に同一かつアトミックに再構築します（`LastUpdate` などの変更されやすいタグもキャプチャ時の状態に復元されます）。

## インストール

このプロジェクトのビルドには Rust ツールチェインが必要です。
リポジトリをクローンし、以下を実行してください:

```sh
cargo install --path ./crates/cli

```

## 使い方

```sh
# 現在のワールド状態を記録（事前にサーバーのセーブを一時停止してください:
# `save-off`、`save-all` を実行し、完了後に `save-on` を実行）
sekai --store ./sekai-store backup ./world

# スナップショットの一覧を表示
sekai --store ./sekai-store list

# スナップショット 1 からワールドを再構築（リージョンファイルを上書き）
sekai --store ./sekai-store rollback ./world 1

# スナップショット 1 と 2 の間でチャンクの NBT を比較
sekai --store ./sekai-store diff 1 2 --in overworld:0,0

# 参照されていない blob のプレビュー後、ガベージコレクションを実行
sekai --store ./sekai-store gc --dry-run
sekai --store ./sekai-store gc

# スナップショット 1 を新しいディレクトリに再構築（稼働中のワールドには影響しません）
sekai --store ./sekai-store export 1 ./restored

# 何も変更せずにリージョンファイルを検証・確認
sekai debug scan ./world

```

すべてのコマンドは `--json` オプションを指定することで、機械読取可能な単一ドキュメントのレポートを出力できます。また、`list` を除くすべてのコマンドは `--timing` オプションでフェーズごとの詳細な処理時間を表示できます（両方を組み合わせることでタイム計測付きの JSON が得られます）。さらに、`backup`、`rollback`、`diff`、`gc` では `--progress` を指定すると標準エラー出力に進捗バーを表示できます（`--json` との併用は不可）。
`backup`、`rollback`、`diff` は対象範囲の絞り込みに対応しています: 繰り返し指定可能な `--in DIM[:x,z|x0,z0..x1,z1]`、`--region DIM:RX,RZ`、および繰り返し指定可能な `--kind`（空の場合はすべて）が利用可能です。指定しない場合はワールド全体が対象となります。出力フォーマットの仕様については `docs/json.md` を参照してください。

> [!Note]
> すべてのスナップショットとストレージの二次バックアップを作成したい場合は、
> `--store` で指定したディレクトリ全体をそのまま外部へコピーまたはアーカイブしてください。

`sekai` はサーバープロセスには一切干渉しません。セーブ調整などの連携は呼び出し側の責任で行ってください。

サーバーのルートディレクトリ（Spigot、Paper、Purpur などの Bukkit 系）または単一のワールドディレクトリ（Vanilla）を指定して実行します。Sekai は Vanilla のレイアウト（`region/`, `DIM-1/`, `DIM1/`, `dimensions/minecraft/...`）、26.1 より前の分割 Bukkit レイアウト、およびカスタムプラグインのワールドフォルダを自動検出します。

設計思想については `ARCHITECTURE.md` を、開発ガイドラインについては `CONTRIBUTING.md` を、詳しい運用方法についてはユーザーガイド（`book/en`、`book/ja`）を参照してください。機械読取可能な出力の仕様は `docs/json.md` に記載されています。

