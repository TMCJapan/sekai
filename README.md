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
cd ./world   # the world directory, or the server root for Bukkit-family servers

# Register the world in a new store at ./.sekai (once, before any other command)
sekai init

# Record the current world state (pause the server first:
# `save-off`, `save-all`, then `save-on` afterwards)
sekai commit

# Preview what a commit would record, without writing anything
sekai status

# Walk the history from HEAD, newest first
sekai log --oneline

# Branch as in Git, then switch to it (rebuilds region files)
sekai branch feature
sekai checkout feature

# Tag a commit, and go back to it later
sekai tag v1
sekai checkout v1

# Compare the world with HEAD, or two commits
sekai diff --in overworld:0,0
sekai diff v1..main --in overworld:0,0

# Save an area once, then select it by name
sekai scope create spawn --in overworld:-8,-8..7,7
sekai commit --scope spawn

# Reclaim space: wipe the rebuildable region-image cache (behavior unchanged)
sekai cache clean
```

Commands work as their Git namesakes do, inside the world they find from
the current directory, as Git finds `.git`. `--world` starts that search
elsewhere, and `--store` uses a store kept outside the world; `init`
creates the store, and every other command fails without one.

Every command accepts `--json` for a single-document machine-readable
report, and `commit`, `status`, `checkout`, `diff`, and `cache update`
accept `--timing` for a per-phase breakdown (combine both for timed
JSON). Progress shows on stderr when it is a terminal; `--no-progress`
turns it off. Commit, status, checkout, and diff accept a scope:
repeatable `--in DIM[:x,z|x0,z0..x1,z1]`, `--region DIM:RX,RZ`, and
repeatable `--kind` (empty means all); nothing selected means the whole
world. `--scope NAME` selects an area saved with `scope create` instead.
See `docs/json.md` for the output contract.

> [!Note]
> If you want to make a full secondary backup of all your commits and storage, 

`sekai` never touches the server process; save coordination belongs to the
caller. 

Run `init` in your server root (for Bukkit-family servers like Spigot, Paper,
or Purpur) or in a single world directory (for Vanilla). Sekai automatically detects
Vanilla layouts (`region/`, `DIM-1/`, `DIM1/`, `dimensions/minecraft/...`),
pre-26.1 split Bukkit layouts, and custom plugin world folders.

See `ARCHITECTURE.md` for the design, `CONTRIBUTING.md` for development
guidelines, and the user guide
([English](https://tmcjapan.github.io/sekai/en/),
[日本語](https://tmcjapan.github.io/sekai/ja/)) for operations.
Machine-readable output is specified in `docs/json.md`.
