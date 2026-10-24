# Ref

```sh
sekai --store ./sekai-store ref list
sekai --store ./sekai-store ref create feature
sekai --store ./sekai-store ref activate feature
sekai --store ./sekai-store ref delete feature
```

ref はコミットを指す名前付きの可変ポインタ（sekai のブランチ）で
す。ref はコミットを指し、新しいコミットがその上に積まれると追従し
ます。コミットを持たない ref は *unborn*（まだコミットのない履歴を指
す状態）です。

**active** な ref はちょうど 1 つです。新規コミットはそこからぶら
下がります。ストアを新規作成すると active な ref が 1 つ（`main`、
unborn）用意されます。`ref list` では `*` が付きます。

- `ref create <name>` は active な ref の指す位置から ref を作りま
  す。コミット（`<id>`、`@tag`、または他の ref 名）を渡すと別の位置を
  指します。既存の ref 名があると `--force` がない限り失敗します。
- `ref activate <name>` は active な ref を切り替えます。次のコミット
  はストア内の最新コミットではなく、この ref を継続します。git の
  `git switch` と同様に履歴が分岐します。
- `ref delete <name>` は ref を削除します。active な ref は削除でき
  ません（削除すると新しいコミットのぶら下げ先が無くなります）。
- ref 名には ASCII 英数字と `.`、`_`、`-`、`/` が使用でき、長さは
  1〜64 バイトです。数字のみの名前は許可されないため、コミット ID と
  誤認されることはありません。`HEAD` はストアの active ref として予約
  されています。

ref は単なるメタデータであり、コミットの保持を強制・拘束するもの
ではありません。`prune` でコミットを削除すると、その ref は fold 先
のコミットを引き継ぎます。
