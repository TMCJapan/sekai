# Sekai

*Read this in [English](README.md).*

ワールドの現在の状態をコミットとして記録し、任意の時点へ素早くチェックアウトします。

Minecraft Java 版のリージョンファイル (`.mca`) を対象とした、チャンク単位で重複排除を行うバージョン管理ツールです。
コミット間で同一のチャンクデータはコンテンツアドレスストレージ (CAS) によって共有されるため、変更がないコミットはストレージ容量をほとんど消費しません。
チェックアウト時は、コミット取得時の生データからリージョンファイルを寸分違わず（`LastUpdate` などの揮発性タグも含め、キャプチャ時の状態通りに）アトミックに再構築します。

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
# ストアを作成（初回の一度だけ。他のコマンドより先に実行）
sekai init

# 現在のワールド状態を記録（事前にサーバーの書き込みを停止してください:
# `save-off`、`save-all` を実行し、完了後に `save-on` で再開）
sekai commit ./world

# ワールドに変更を加えずに、コミットで何が記録されるかをプレビュー
sekai status ./world

# コミットの一覧を表示
sekai log


# 特定のコミットからワールドを再構築（リージョンファイルを上書き）
sekai checkout ./world 2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a

# 特定のコミットからワールドを再構築（リージョンファイルを上書き）
# 特定のコミットからワールドを再構築（リージョンファイルを上書き）

# 2 つのコミット間でチャンクの NBT を比較（ハッシュは `log` で確認）
sekai diff 1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b 2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a --in overworld:0,0

# ブランチ: 現在位置の名前付けを行い、そのブランチへコミットする
sekai ref create feature
sekai ref activate feature

# 範囲を一度保存し、以後は名前で選択する
sekai scope create spawn --in overworld:-8,-8..7,7
sekai commit ./world --scope spawn

# 参照されていない Blob をプレビュー後、ガベージコレクション (GC) で回収
sekai gc --dry-run
sekai gc

# 古いコミットを削除（最新 10 件を残す）し、不要になった Blob を回収
sekai prune --keep-last 10
sekai gc
```

`--store` はストアディレクトリを指定します（`init` が作成し、他のコマンドはストアが存在しない場合に失敗します）。省略時のデフォルトは `.sekai` です。

すべてのコマンドは `--json` オプションに対応しており、単一の機械可読な JSON レポートを出力できます。また、`log`、`ref`、`init` を除くすべてのコマンドは `--timing` オプションでフェーズごとの処理時間内訳を表示できます（両方を組み合わせることで、所要時間付きの JSON が得られます）。さらに、`commit`、`status`、`checkout`、`diff`、`gc`、`prune` では `--progress` を指定すると標準エラー出力に進捗バーを表示できます（`--json` との併用は不可）。

`commit`、`status`、`checkout`、`diff` はスコープ（対象範囲）の絞り込みに対応しています。複数指定可能な `--in DIM[:x,z|x0,z0..x1,z1]`、`--region DIM:RX,RZ`、および複数指定可能な `--kind`（省略時はすべて）が利用可能です。スコープを指定しない場合は、ワールド全体が対象となります。`--scope NAME` を使うと、`scope create` で保存した範囲を名前で選択できます。出力フォーマットの仕様については `docs/json.md` を参照してください。

> [!Note]
> すべてのコミットとストレージの二次バックアップを作成したい場合は、

`sekai` はサーバープロセスを直接操作しません。コミット前後のセーブ停止や再開などの連携処理は、呼び出し側（スクリプトや管理者）で行ってください。

引数にはサーバーのルートディレクトリ（Spigot、Paper、Purpur などの Bukkit 系）または単一のワールドディレクトリ（Vanilla）を指定します。Sekai は Vanilla のレイアウト（`region/`, `DIM-1/`, `DIM1/`, `dimensions/minecraft/...`）、26.1 より前の Bukkit 分割レイアウト、およびカスタムプラグインのワールドフォルダを自動検出します。

設計思想については `ARCHITECTURE.md` を、開発ガイドラインについては `CONTRIBUTING.md` を、詳しい運用方法についてはユーザーガイド（[English](https://tmcjapan.github.io/sekai/en/)、[日本語](https://tmcjapan.github.io/sekai/ja/)）を参照してください。機械可読な出力の仕様は `docs/json.md` に記載されています。
