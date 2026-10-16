# World layouts

Four server layouts share the `.mca` format but not the directory layout.
Pass the same path on every run:

- **Vanilla 26.1+**: a single world folder holding
  `dimensions/minecraft/<name>/`.
- **Vanilla legacy**: a single world folder (`region/`, `DIM-1/`,
  `DIM1/`).
- **Bukkit 26.1+ (Paper)**: the server root, whose `<base>/` folder holds
  `dimensions/minecraft/<name>/` (`base` is the `level-name`, `world` by
  default).
- **Bukkit legacy** (Bukkit/Spigot/Paper/Purpur, pre-26.1): the server
  root, holding `<base>/`, `<base>_nether/DIM-1/`, and
  `<base>_the_end/DIM1/`.
- **Plugin worlds** (Multiverse et al.): arbitrary world folders beside
  the elected one, detected by their contents.

Opening a path elects exactly one layout, narrow signature first, and only
that layout is discovered: leftovers from the others (conversion residue,
a stray `dimensions/` tree) stay invisible instead of being merged.
Datapack dimensions live under the world's `dimensions/<ns>/<name>` tree
in every layout, and plugin worlds each elect their own single layout.

The full namespace rules live in
[ARCHITECTURE.md](https://github.com/tmcjapan/sekai/blob/main/ARCHITECTURE.md) ("World Layouts"). Two practical
consequences: custom-dimension folders are content-hashed, so renaming
one orphans its history; and rollback restores moved folders through
discovered siblings when possible, failing loudly
(`UnknownRegionPath`) rather than writing somewhere wrong.
