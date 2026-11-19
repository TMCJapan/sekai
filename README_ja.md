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
cd ./world   # ワールドのディレクトリ（Bukkit 系サーバーではサーバーのルート）

# ワールドを ./.sekai の新しいストアに登録（初回の一度だけ。他のコマンドより先に実行）
sekai init

# 現在のワールド状態を記録（事前にサーバーの書き込みを停止してください:
# `save-off`、`save-all` を実行し、完了後に `save-on` で再開）
sekai commit

# ワールドに変更を加えずに、コミットで何が記録されるかをプレビュー
sekai status

# HEAD から新しい順に履歴をたどる
sekai log --oneline

# Git と同じようにブランチを作り、切り替える（リージョンファイルを再構築）
sekai branch feature
sekai checkout feature

# コミットにタグを付け、あとでそこに戻る
sekai tag v1
sekai checkout v1

# ワールドと HEAD、または2つのコミットを比較
sekai diff --in overworld:0,0
sekai diff v1..main --in overworld:0,0

# 範囲を一度保存し、以後は名前で選択する
sekai scope create spawn --in overworld:-8,-8..7,7
sekai commit --scope spawn

# 容量を削減: 再構築可能なリージョンイメージキャッシュを削除（挙動は変わらない）
sekai cache clean
```

各コマンドは Git の同名のコマンドと同じように動作し、Git が `.git` を探すのと同じく、カレントディレクトリから見つけたワールドの中で動きます。`--world` を指定すると別の場所から探し始め、`--store` を指定するとワールドの外に置いたストアを使います。ストアを作成するのは `init` で、他のコマンドはストアがないと失敗します。

すべてのコマンドは `--json` オプションに対応しており、単一の機械可読な JSON レポートを出力できます。また、`commit`、`status`、`checkout`、`diff`、`cache update` は `--timing` オプションでフェーズごとの処理時間内訳を表示できます（両方を組み合わせることで、所要時間付きの JSON が得られます）。進捗は標準エラー出力が端末のときに表示され、`--no-progress` で止められます。

`commit`、`status`、`checkout`、`diff` はスコープ（対象範囲）の絞り込みに対応しています。複数指定可能な `--in DIM[:x,z|x0,z0..x1,z1]`、`--region DIM:RX,RZ`、および複数指定可能な `--kind`（省略時はすべて）が利用可能です。スコープを指定しない場合は、ワールド全体が対象となります。`--scope NAME` を使うと、`scope create` で保存した範囲を名前で選択できます。出力フォーマットの仕様については `docs/json.md` を参照してください。

> [!Note]
> すべてのコミットとストレージの二次バックアップを作成したい場合は、

`sekai` はサーバープロセスを直接操作しません。コミット前後のセーブ停止や再開などの連携処理は、呼び出し側（スクリプトや管理者）で行ってください。

`init` は、サーバーのルートディレクトリ（Spigot、Paper、Purpur などの Bukkit 系）または単一のワールドディレクトリ（Vanilla）で実行します。Sekai は Vanilla のレイアウト（`region/`, `DIM-1/`, `DIM1/`, `dimensions/minecraft/...`）、26.1 より前の Bukkit 分割レイアウト、およびカスタムプラグインのワールドフォルダを自動検出します。

設計思想については `ARCHITECTURE.md` を、開発ガイドラインについては `CONTRIBUTING.md` を、詳しい運用方法についてはユーザーガイド（[English](https://tmcjapan.github.io/sekai/en/)、[日本語](https://tmcjapan.github.io/sekai/ja/)）を参照してください。機械可読な出力の仕様は `docs/json.md` に記載されています。
