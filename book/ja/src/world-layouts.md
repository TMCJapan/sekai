# ワールドレイアウト

`.mca` フォーマット自体は共通ですが、サーバーの種類ごとにディレクトリ配置（レイアウト）は異なります。実行時は常に同じ対象パスを指定してください：

- **バニラ (Vanilla)**: 1つのワールドディレクトリを指定します（中に `region/`, `DIM-1/`, `DIM1/`、または 26.1 以降では `dimensions/minecraft/<name>/` が存在するディレクトリ）。
- **Bukkit 系**（Bukkit / Spigot / Paper / Purpur の 26.1 より前のレイアウト）: サーバールートディレクトリを指定します。`<base>/`, `<base>_nether/DIM-1/`, `<base>_the_end/DIM1/` が配置されています（ここで `<base>` は `level-name` の値で、デフォルトは `world` です）。※ Paper 26.1+ ではバニラレイアウトへ移行します。
- **プラグインワールド**（Multiverse 等）: ファイル内容に基づいて自動検出される任意のフォルダ。

ネームスペースに関する完全な命名規則は [ARCHITECTURE.md](https://github.com/tmcjapan/sekai/blob/main/ARCHITECTURE.md) の「World Layouts」セクションを参照してください。

運用上の実用的な注意点は以下の2点です：
1. バニラのディメンションおよび `dimensions/<ns>/<name>` ツリーは、レイアウト移行後も名前空間ID（例: `minecraft:overworld`, `aether:sky`）を維持するため、履歴が継続します。
2. フォルダが移動されていた場合、ロールバックは検出された兄弟フォルダ（同階層のフォルダ構造）を手がかりに可能な限り移動先を解決して復元します。ただし、誤った場所に書き込むリスクを避けるため、復元先が特定できない場合はエラー（`UnknownRegionPath`）を出して安全に中断します。
