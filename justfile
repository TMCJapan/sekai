# Default task: run all checks
default: check

# Run all checks required for PR / local validation
check: fmt-check clippy check-core test deny

# Format code
fmt:
    cargo fmt --all

# Check code formatting
fmt-check:
    cargo fmt --all -- --check

# Run linter
clippy:
    cargo clippy --all-targets -- -D warnings

# Ensure crates/core remains no_std and dependency-free
check-core:
    cargo check -p sekai-core --target thumbv7m-none-eabi --no-default-features

# Run all unit and integration tests
test:
    cargo test --all

# Run cargo-deny checks (licenses, advisories, bans)
deny:
    cargo deny check
