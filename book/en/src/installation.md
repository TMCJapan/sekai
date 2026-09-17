# Installation

A Rust toolchain is required to build from source (the pinned version is
in `rust-toolchain.toml`). Clone the repository and run:

```sh
cargo install --path ./crates/cli
```

The installed binary is named `sekai`. Release builds for all supported
platforms are also produced as CI artifacts (`sekai-<target triple>`).

Verify the setup:

```sh
sekai --help
```
