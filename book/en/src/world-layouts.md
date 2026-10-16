# World layouts

Three server families share the `.mca` format but not the directory
layout. Sekai reads the path you pass through one detection step, turns
the result into a *dimension map* (one directory per observed dimension),
and everything else — backup, rollback, diff, and export — works from that
map. Pass the same path on every run.

## Layouts

- **Vanilla**: one world folder (`region/`, `DIM-1/`, `DIM1/`, and since
  26.1 `dimensions/minecraft/<name>/`).
- **Bukkit-family** (Bukkit/Spigot/Paper/Purpur, pre-26.1 layout): the
  server root, holding `<base>/`, `<base>_nether/DIM-1/`, and
  `<base>_the_end/DIM1/`, where `base` is the `level-name` (`world` by
  default). Each datapack dimension `<namespace>:<path>` lives in its own
  sibling folder `<base>_<namespace>_<path>/dimensions/<namespace>/<path>/`.
  Paper 26.1+ migrates to the vanilla layout.
- **Plugin worlds** (Multiverse et al.): arbitrary folders, detected by
  their contents.

## Detection

Opening a world classifies the root once:

1. Sekai reads `server.properties` at the root, when present, and takes
   the first uncommented `level-name=` line; the value is trimmed, and a
   missing, unreadable, or unusable value counts as "no name" — unusable
   means empty, or anything but one plain directory name (no separators,
   no `.`/`..`), so a corrupted file cannot point the layout outside the
   observed root.
2. A directory counts as a *world folder* when `region/`, `entities/`, or
   `poi/` inside it holds at least one parseable `r.<x>.<z>.mca` file.
3. Candidates are checked in order and the first match wins:

```text
level-name found?
  ├─ <base>/dimensions/minecraft/<name>/ is a world folder?  → Bukkit, 26.1 layout
  └─ <base>/, <base>_nether/DIM-1, <base>_the_end/DIM1?      → Bukkit, split layout
dimensions/minecraft/<name>/ is a world folder?              → Vanilla, 26.1 layout
region/, DIM-1/, or DIM1/ is a world folder?                 → Vanilla, legacy layout
none of the above                                            → empty legacy layout
```

Only the first matching layout is observed: leftovers of another layout
(for example legacy folders next to a 26.1 `dimensions/` tree) stay out of
discovery. A root that matches nothing yet becomes an empty legacy-layout
world; discovery of a missing root still fails loudly. A directory that
cannot be inspected (permission denied, or a kind directory that cannot
be listed) fails the open loudly as well, instead of silently changing
the observed layout.

## The dimension map

Detection picks the layout; collection then resolves one directory per
observed dimension:

```text
Bukkit, 26.1     every <namespace>/<name> under <base>/dimensions/
Bukkit, split    <base>/ (overworld), <base>_nether/DIM-1, <base>_the_end/DIM1,
                 plus every <namespace>/<name> under each sibling's dimensions/ tree
Vanilla, 26.1    every <namespace>/<name> under dimensions/
Vanilla, legacy  region/ (overworld), DIM-1, DIM1,
                 plus dimensions/<namespace>/<name> when present (pre-migration datapacks)
```

- The split layout's sibling folder name is ambiguous (`<namespace>` and
  `<path>` may both contain `_`), so a datapack dimension takes its
  identity from the `dimensions/` tree inside the folder, not from the
  folder name.
- Empty dimension directories stay in the map, even before their first
  region file appears.
- Each dimension is keyed by a stable hash of its canonical
  `<namespace>:<name>` id. The vanilla trio always uses
  `minecraft:overworld`, `minecraft:the_nether`, and `minecraft:the_end`,
  wherever its files live, so a world that migrates between layouts (say,
  Paper 26.1 flattening the split layout into `dimensions/`) keeps the
  same ids — and one continuous history. Renaming the world folder does
  not change ids; renaming a dimension directory (or its namespace)
  starts a new history for that dimension.

## Discovery

For every dimension in the map, sekai walks the three region families and
yields the files a backup or rollback operates on:

```text
dimension dir ─▶ region/ entities/ poi/
              ─▶ entries sorted by name
              ─▶ keep r.<x>.<z>.mca files, first file per coordinate wins
              ─▶ RegionRef { path, id, kind, x, z }
```

- Name order decides ties: two files spelling the same coordinates
  (`r.0.0.mca` and `r.00.00.mca`) resolve the same way on every run.
- Symlinks are followed, like the game does; a missing kind directory is
  read as empty, but a missing world root or an unreadable directory fails
  loudly instead of silently dropping a dimension.
- Rollback and export derive their target paths from the map; when folders
  have moved since the backup, a missing region file is restored next to a
  live sibling of the same dimension, and when no observed location exists
  the run fails loudly (`UnknownRegionPath`) rather than writing somewhere
  wrong.

The wider architectural summary lives in
[ARCHITECTURE.md](https://github.com/tmcjapan/sekai/blob/main/ARCHITECTURE.md)
("World Layouts").
