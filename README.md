# Sekai

*Read this in [日本語](README_ja.md).*

Save the current state of the world as a commit and check out any of them quickly.

Chunk-level deduplicated version control for Minecraft Java region files (`.mca`).
Commits share identical chunk payloads via content-addressed storage, and
checkout rebuilds region files from the commit's captured payloads,
verbatim and atomically (volatile tags such as `LastUpdate` are rewound
to their capture-time values).

## Installation

Prebuilt binaries are attached to each
[GitHub Release](https://github.com/tmcjapan/sekai/releases)
(`sekai-<target triple>.tar.gz`). Alternatively, with a Rust toolchain
installed:

```sh
cargo install --git https://github.com/tmcjapan/sekai

```

The installed binary is named `sekai`.

## Usage

```sh
# Record the current world state (pause the server first:
# `save-off`, `save-all`, then `save-on` afterwards)
sekai commit ./world

# Preview what a commit would record, without writing anything
sekai status ./world

# List commits
sekai log

# Rebuild the world from a known commit (overwrites region files)
sekai checkout ./world 2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a

# Compare chunk NBT between two commits (hashes from `log`)
sekai diff 1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b 2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a --in overworld:0,0

# Branch: name the current position, then commit onto it
sekai ref create feature
sekai ref activate feature

# Preview unreferenced blobs, then collect them
sekai gc --dry-run
sekai gc

# Delete old commits (keep newest 10), then reclaim their blobs
sekai prune --keep-last 10
sekai gc

# Rebuild a commit into a fresh directory (live world untouched)
sekai clone 2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a ./restored

```

missing; every other command fails on a missing store) and defaults
to `.sekai` when omitted.

Every command accepts `--json` for a single-document machine-readable
report, and every command except `log` and `ref` accepts
`--timing`
for a per-phase breakdown (combine both for timed JSON). `commit`,
`status`, `checkout`, `diff`, `clone`, `gc`, and `prune` also take
`--progress` for a stderr progress bar (refused with `--json`).
Commit, status, checkout, diff,
and clone accept a scope: repeatable `--in DIM[:x,z|x0,z0..x1,z1]`,
`--region DIM:RX,RZ`, and repeatable `--kind` (empty means all);
nothing selected means the whole world. See `docs/json.md` for the
output contract.

The git-style names are canonical; the previous ones still work as
aliases: `backup` (`commit`), `rollback` (`checkout`), `export`
(`clone`), and `list` (`log`).

> [!Note]
> If you want to make a full secondary backup of all your commits and storage, 

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
