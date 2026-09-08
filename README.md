# Sekai

*Read this in [日本語](README_ja.md).*

Save the current state of the world as a snapshot and roll back to any of them quickly.

Chunk-level deduplicated backups for Minecraft Java region files (`.mca`).
Snapshots share identical chunk payloads via content-addressed storage, and
rollback rebuilds region files byte-identically and atomically.

## Installation

Rust toolchain is required to build this project.
Clone this repository and run:

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

`sekai` never touches the server process; save coordination belongs to the
caller. Both legacy (`region/`, `DIM-1/`, `DIM1/`) and modern
(`dimensions/minecraft/...`) world layouts are discovered automatically.
See `ARCHITECTURE.md` for the design and `CONTRIBUTING.md` for development
guidelines.
