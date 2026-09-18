# Installation

Prebuilt binaries are attached to each GitHub Release
(`sekai-<target triple>.tar.gz`). Alternatively, with a Rust toolchain
installed (the pinned version is in `rust-toolchain.toml`):

```sh
cargo install sekai-cli
```

The installed binary is named `sekai`. To build from a checkout instead
(for development):

```sh
cargo install --path ./crates/cli
```

Verify the setup:

```sh
sekai --help
```
