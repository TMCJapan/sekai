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
sekai --store ./sekai-store commit ./world

# Preview what a commit would record, without writing anything
sekai --store ./sekai-store status ./world

# List commits
sekai --store ./sekai-store log

# Rebuild the world from commit 1 (overwrites region files)
sekai --store ./sekai-store checkout ./world 1

# Compare chunk NBT between commits 1 and 2
sekai --store ./sekai-store diff 1 2 --in overworld:0,0

# Name commit 2 for later reference
sekai --store ./sekai-store tag create stable 2

# Branch: name the current position, then commit onto it
sekai --store ./sekai-store ref create feature
sekai --store ./sekai-store ref activate feature

# Preview unreferenced blobs, then collect them
sekai --store ./sekai-store gc --dry-run
sekai --store ./sekai-store gc

# Delete old commits (keep newest 10), then reclaim their blobs
sekai --store ./sekai-store prune --keep-last 10
sekai --store ./sekai-store gc

# Rebuild commit 1 into a fresh directory (live world untouched)
sekai --store ./sekai-store clone 1 ./restored

```

Every command accepts `--json` for a single-document machine-readable
report, and every command except `log`, `tag`, and `ref` accepts
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
