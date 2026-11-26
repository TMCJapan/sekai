# ADR-0040: Store config as typed TOML

- Status: Accepted
- Date: 2026-11-27

## Context

Sekai needs git-style settings (starting with the commit author name,
`user.name`) that scripts and users can read and write from the CLI.
Git layers system/global/local scopes; sekai has no global concept -
the store is the only scope - so the file belongs to the store root
beside `version` and the databases. The binary stays a thin clap
wrapper, so the file format and its I/O belong to a library crate.
Store operations already share one exclusive open (the sled session
locks, ADR-0024/ADR-0029); config is a store operation and takes the
same path rather than a second, location-based entry point that would
bypass the opener's link resolution and version gate.

## Decision

- `<root>/config.toml` holds the config as plain TOML over typed serde
  structs in `sekai-storage-local`: `Config { user: User }`,
  `User { name: String }`. Every field defaults, an empty string reads
  as unset and is never serialized, and unknown sections are tolerated
  on read. The file format itself is independent of `STORE_VERSION`:
  adding a key does not bump the store format.
- `LocalStore::read_config(&self)` / `write_config(&self, &Config)`
  operate on an opened store's root. The caller opens the store as for
  every operation: a store link resolves to the real root, a missing
  store fails with `StoreMissing`, and the version gate applies. The
  write goes to a temporary sibling plus `fsync` plus `rename`, so a
  crash never leaves a torn config.
- `ConfigKey` enumerates the known keys (`user.name` today); parsing is
  case-insensitive and refuses unknown keys at CLI parse time
  (ADR-0013). `Config::get`/`set`/`unset`/`entries` operate on that enum,
  so a new key is one variant plus one field, never a stringly-typed
  lookup.
- `sekai config` adopts the git argument subset that fits typed
  single-valued keys: `config <key>` (get), `config <key> <value>`
  (set), `--get`, `--unset`, `--list`/`-l`, `--name-only` (human-only,
  refused with `--json`), and `--default <value>` for reads, plus the
  project-wide `--json`. Location flags (`--global`, `--system`,
  `--local`, `--file`), multivar actions (`--add`, `--replace-all`,
  `--get-all`, `--unset-all`), regex/section operations, and `--edit`
  are not adopted. An empty value is refused at parse time; use
  `--unset`.
- A get or unset on an unset key fails (exit 1) with
  `config key '<key>' is not set`; git's distinct exit codes (1 for
  get, 5 for unset) collapse into the project's 0/1/2 scheme. Set and
  unset are silent on success in human mode, like git; reads print the
  value verbatim, unstyled.
- New dependency: `toml` (MIT OR Apache-2.0) in `sekai-storage-local`
  only, plus the workspace `serde`. The pure layers stay untouched.

## Consequences

Config shares the store's single-opener exclusion: a config edit and a
running backup cannot hold the store at the same time, and both go
through the same link resolution and version gate as every operation.
Library callers read the same typed struct the CLI writes. The typed
model costs git's free-form extensibility: a key must exist in the
enum, and unknown sections written by a newer binary are dropped on
the next read-modify-write cycle (pre-release policy: recreate, don't
migrate). A future backend defines its own config surface, as with
worktrees (ADR-0038). Wiring `user.name` into commit authorship is a
follow-up; this change only stores and reports it.