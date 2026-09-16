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

```

> [!Note]
> If you want to perform a full secondary backup of all your snapshots and storage, 
> simply copy or archive the entire directory specified by `--store` to an external location.

`sekai` never touches the server process; save coordination belongs to the
caller. 

Pass your server root (for Bukkit-family servers like Spigot, Paper, or Purpur)
or a single world directory (for Vanilla). Sekai automatically detects
Vanilla layouts (`region/`, `DIM-1/`, `DIM1/`, `dimensions/minecraft/...`),
pre-26.1 split Bukkit layouts, and custom plugin world folders.

See `ARCHITECTURE.md` for the design and `CONTRIBUTING.md` for development
guidelines.
