# World layouts

Three server families share the `.mca` format but not the directory
layout. Pass the same path on every run:

- **Vanilla**: one world folder (`region/`, `DIM-1/`, `DIM1/`, and since
  26.1 `dimensions/minecraft/<name>/`).
- **Bukkit-family** (Bukkit/Spigot/Paper/Purpur, pre-26.1 layout): the
  server root, holding `<base>/`, `<base>_nether/DIM-1/`, and
  `<base>_the_end/DIM1/`, where `base` is the `level-name` (`world` by
  default). Paper 26.1+ migrates to the vanilla layout.
- **Plugin worlds** (Multiverse et al.): arbitrary folders, detected by
  their contents.

The full key rules live in
[ARCHITECTURE.md](https://github.com/tmcjapan/sekai/blob/main/ARCHITECTURE.md) ("World Layouts"). Two practical
consequences: vanilla dimensions and `dimensions/<ns>/<name>` trees keep
their namespaced ids (`minecraft:overworld`, `aether:sky`) across layout
migrations, so their history survives moves; and rollback restores moved
folders through discovered siblings when possible, failing loudly
(`UnknownRegionPath`) rather than writing somewhere wrong.
