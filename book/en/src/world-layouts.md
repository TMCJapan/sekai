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

The full namespace rules live in
[ARCHITECTURE.md](../../ARCHITECTURE.md) ("World Layouts"). Two practical
consequences: custom-dimension folders are content-hashed, so renaming
one orphans its history; and rollback restores moved folders through
discovered siblings when possible, failing loudly
(`UnknownRegionPath`) rather than writing somewhere wrong.
