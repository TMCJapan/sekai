# 検証・検査

これらのコマンドは、ワールドにもストアにも書き込みません。

```sh
sekai status
sekai log --oneline --decorate
sekai log --all --stat
sekai diff --in overworld:0,0
sekai diff v1..main --in overworld:0,0..31,31
```

- `status` は Git と同じ1行目（`On branch <name>` や `HEAD detached at <id>`（HEAD のコミット））から始まり、コミットで記録される内容をプレビューします。基準となるコミット、変更されたリージョン（新規ファイルと削除されたファイルは別に集計）、新たに記録されるチャンク、ツームストーン、CAS にない Blob を表示します。`clean` はスコープ内に HEAD のコミットとの違いがないことを表します。途中でワールドが変わらない限り、件数はその後のコミットと一致します。`commit --dry-run` は意図的に用意していません。`status` がその役割を担います。
- `log` は HEAD から親を新しい順にたどり、Git の `medium` 形式から sekai が記録しない作者とメッセージを除いた形で表示します。`--oneline` はコミットごとに1行、`--reverse` は古い順、`--all` はすべての ref から表示します。`v1..main` のようなリビジョンや範囲で、たどる対象を選べます。`(HEAD -> main, tag: v1)` のような装飾は端末のときに表示され、`--decorate` と `--no-decorate` で切り替えられます。`--stat` はコミットごとの変更統計を追加します。まだコミットのないブランチでは、Git と同じく失敗します。
- `diff` は、ワールドと HEAD、ワールドと1つの commit-ish、または2つのコミット（`diff a b` または `diff a..b`）を比較します。`--in DIM:x,z` を1つだけ指定すると単一チャンクの出力になり、複数指定すると差分のないチャンクを省いたグループ化された出力になります。スコープ指定（`--in`、`--region`、`--kind`）で比較するチャンクを選びます。[スコープの選択](../scope.md)を参照してください。一方の側に存在しない、またはツームストーンのチャンクは、その側では空のコンパウンドとして比較されます。存在しない座標がエラーになることはありません（[ADR-0006](https://github.com/tmcjapan/sekai/blob/main/docs/adr/0006-diff-missing-as-empty.md) を参照）。明確に失敗するのは、破損したペイロードと Blob の欠落だけです。まだコミットのないブランチでワールドと比較すると、Git の `diff HEAD` と同じく失敗します。
