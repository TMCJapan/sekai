# Sekai

*Read this in [English](README.md).*

ワールドの現在の状態をスナップショットとして記録し、任意の時点へ素早くロールバックします。

Minecraft Java 版のリージョンファイル (`.mca`) を対象とした、チャンク単位で重複排除を行うバックアップツールです。
スナップショット間で同一のチャンクデータはコンテンツアドレスストレージ (CAS) によって共有されるため、変更がないバックアップはストレージ容量をほとんど消費しません。
ロールバック時は、スナップショット取得時の生データからリージョンファイルを寸分違わず（`LastUpdate` などの揮発性タグも含め、キャプチャ時の状態通りに）アトミックに再構築します。

## インストール

各 [GitHub Release](https://github.com/tmcjapan/sekai/releases) にて、
プリビルトバイナリ（`sekai-<target triple>.tar.gz`）を配布しています。
Rust ツールチェーンが導入されている環境であれば、Cargo を使って直接インストールすることも可能です:

```sh
cargo install --git https://github.com/tmcjapan/sekai
```

インストールされるバイナリ名は `sekai` です。

## 使い方

```sh
# 現在のワールド状態を記録（事前にサーバーの書き込みを停止してください:
# `save-off`、`save-all` を実行し、完了後に `save-on` で再開）
sekai --store ./sekai-store backup ./world

# ワールドに変更を加えずに、バックアップで何が記録されるかをプレビュー
sekai --store ./sekai-store status ./world

# スナップショットの一覧を表示
sekai --store ./sekai-store list

# スナップショット 1 からワールドを再構築（リージョンファイルを上書き）
sekai --store ./sekai-store rollback ./world 1

# スナップショット 1 と 2 の間でチャンクの NBT を比較
sekai --store ./sekai-store diff 1 2 --in overworld:0,0

# スナップショット 2 に後から参照できるタグ名を付与
sekai --store ./sekai-store tag create stable 2

# 参照されていない Blob をプレビュー後、ガベージコレクション (GC) で回収
sekai --store ./sekai-store gc --dry-run
sekai --store ./sekai-store gc

# 古いスナップショットを削除（最新 10 件を残す）し、不要になった Blob を回収
sekai --store ./sekai-store prune --keep-last 10
sekai --store ./sekai-store gc

# スナップショット 1 を新しいディレクトリに復元（稼働中のワールドには影響しません）
sekai --store ./sekai-store export 1 ./restored

# データを変更せずにリージョンファイルを検証・検査
sekai debug scan ./world
```

すべてのコマンドは `--json` オプションに対応しており、単一の機械可読な JSON レポートを出力できます。また、`list` と `tag` を除くすべてのコマンドは `--timing` オプションでフェーズごとの処理時間内訳を表示できます（両方を組み合わせることで、所要時間付きの JSON が得られます）。さらに、`backup`、`status`、`rollback`、`diff`、`export`、`gc`、`prune` では `--progress` を指定すると標準エラー出力に進捗バーを表示できます（`--json` との併用は不可）。

`backup`、`status`、`rollback`、`diff`、`export` はスコープ（対象範囲）の絞り込みに対応しています。複数指定可能な `--in DIM[:x,z|x0,z0..x1,z1]`、`--region DIM:RX,RZ`、および複数指定可能な `--kind`（省略時はすべて）が利用可能です。スコープを指定しない場合は、ワールド全体が対象となります。出力フォーマットの仕様については `docs/json.md` を参照してください。

> [!Note]
> すべてのスナップショットとストレージの二次バックアップを作成したい場合は、
> `--store` で指定したディレクトリ全体をそのまま外部ストレージへコピーまたはアーカイブしてください。

`sekai` はサーバープロセスを直接操作しません。バックアップ前後のセーブ停止や再開などの連携処理は、呼び出し側（スクリプトや管理者）で行ってください。

引数にはサーバーのルートディレクトリ（Spigot、Paper、Purpur などの Bukkit 系）または単一のワールドディレクトリ（Vanilla）を指定します。Sekai は Vanilla のレイアウト（`region/`, `DIM-1/`, `DIM1/`, `dimensions/minecraft/...`）、26.1 より前の Bukkit 分割レイアウト、およびカスタムプラグインのワールドフォルダを自動検出します。

設計思想については `ARCHITECTURE.md` を、開発ガイドラインについては `CONTRIBUTING.md` を、詳しい運用方法についてはユーザーガイド（[English](https://tmcjapan.github.io/sekai/en/)、[日本語](https://tmcjapan.github.io/sekai/ja/)）を参照してください。機械可読な出力の仕様は `docs/json.md` に記載されています。
