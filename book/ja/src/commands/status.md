# ステータス

```sh
sekai --store ./sekai-store status ./world
```

現在のワールドでバックアップを取ると何が記録されるかをプレビューします。CAS への put もメタデータの commit もなし。ワールドとストアの双方に対して read-only です。

- 基準スナップショット（`latest`、ストア空時は `null`）、変化 region（新規・削除ファイルを分記）、新規記録されるチャンク、tombstone、CAS 不在の blob を報告します。
- `clean` はスコープ内が最新スナップショットと無差分である意味です。スコープ付き実行は範囲内の clean のみ報告します。
- `--jobs N` でプレビューのワーカー数（`0` は CPU 数）。diff ビューは常時 skip（プレビュー無関係の decode のため）。
- ワールド不変なら直後 backup の件数と一致します。

`backup --dry-run` はなく、`status` がその役割を担う設計です。
