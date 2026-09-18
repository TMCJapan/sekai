# ワールドのレイアウト

3つのサーバーファミリーは `.mca` フォーマットを共有していますが、ディレクトリ構造（レイアウト）は共有していません。
毎回の実行時に同じパスを指定してください：

- **バニラ (Vanilla)**: 1つのワールドフォルダ（`region/`, `DIM-1/`, `DIM1/`、および 26.1 以降は `dimensions/minecraft/<name>/`）。
- **Bukkit ファミリー** (Bukkit/Spigot/Paper/Purpur、26.1 以前のレイアウト): サーバーのルートディレクトリ。`<base>/`, `<base>_nether/DIM-1/`, `<base>_the_end/DIM1/` を保持します（ここで `base` は `level-name`、デフォルトは `world`）。Paper 26.1+ はバニラレイアウトへ移行します。
- **プラグインワールド** (Multiverse 等): その内容によって検出される任意のフォルダ。

完全なネームスペース規則は [ARCHITECTURE.md](https://github.com/TMCJapan/sekai/blob/main/ARCHITECTURE.md) ("World Layouts") に記載されています。
実用上の2つの注意点：カスタムディメンションのフォルダはコンテンツハッシュ化されているため、名前を変更するとその履歴が孤立します。
また、ロールバックは誤った場所に書き込むのではなく、明確にエラー（`UnknownRegionPath`）を出力する前に、可能な限り検出された同階層のフォルダを通じて移動されたフォルダを復元します。
