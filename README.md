# Sekai

*Read this in [日本語](README_ja.md).*

Save the current state of the world as a snapshot and roll back to any of them quickly.

Chunk-level deduplicated backups for Minecraft Java region files (`.mca`).
Snapshots share identical chunk payloads via content-addressed storage, and
rollback rebuilds region files from the snapshot's captured payloads,
verbatim and atomically (volatile tags such as `LastUpdate` are rewound
to their capture-time values).

## Installation

Rust toolchain is required to build this project.
Clone the repository and run:

```sh
cargo install --path ./crates/cli

```

## Usage

```sh
# Record the current world state (pause the server first:
# `save-off`, `save-all`, then `save-on` afterwards)
sekai --store ./sekai-store backup ./world

# List snapshots
sekai --store ./sekai-store list

# Rebuild the world from snapshot 1 (overwrites region files)
sekai --store ./sekai-store rollback ./world 1

# Compare chunk NBT between snapshots 1 and 2
sekai --store ./sekai-store diff 1 2 --in overworld:0,0

# Preview unreferenced blobs, then collect them
sekai --store ./sekai-store gc --dry-run
sekai --store ./sekai-store gc

# Rebuild snapshot 1 into a fresh directory (live world untouched)
sekai --store ./sekai-store export 1 ./restored

# Inspect region files without touching anything
sekai debug scan ./world

```

Every command accepts `--json` for a single-document machine-readable
report, and every command except `list` accepts `--timing` for a
per-phase breakdown (combine both for timed JSON). `backup`, `rollback`,
`diff`, and `gc` also take `--progress` for a stderr progress bar
(refused with `--json`). Backup, rollback,
and diff accept a scope: repeatable `--in DIM[:x,z|x0,z0..x1,z1]`,
`--region DIM:RX,RZ`, and repeatable `--kind` (empty means all);
nothing selected means the whole world. See `docs/json.md` for the
output contract.

> [!Note]
> If you want to perform a full secondary backup of all your snapshots and storage, 
> simply copy or archive the entire directory specified by `--store` to an external location.

`sekai` never touches the server process; save coordination belongs to the
caller. 

Pass your server root (for Bukkit-family servers like Spigot, Paper, or Purpur)
or a single world directory (for Vanilla). Sekai automatically detects
Vanilla layouts (`region/`, `DIM-1/`, `DIM1/`, `dimensions/minecraft/...`),
pre-26.1 split Bukkit layouts, and custom plugin world folders.

See `ARCHITECTURE.md` for the design, `CONTRIBUTING.md` for development
guidelines, and the user guide
([English](https://tmcjapan.github.io/sekai/en/),
[日本語](https://tmcjapan.github.io/sekai/ja/)) for operations.
Machine-readable output is specified in `docs/json.md`.
