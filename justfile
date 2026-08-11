set shell := ["bash", "-euo", "pipefail", "-c"]

# List available workflows
default:
    @just --list

# Build the debug binary
build:
    cargo build

# Run all tests
test:
    cargo test

# Check formatting, lints, and tests
check:
    cargo fmt -- --check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo test

# Format Rust sources
fmt:
    cargo fmt

# Build an optimized release binary
release:
    cargo build --release

# Install work-tracker for the current user
install:
    cargo install --path . --locked

# Uninstall work-tracker for the current user
uninstall:
    cargo uninstall work-tracker

# Run the read-only dashboard; pass options after `--`, if needed
serve *args:
    cargo run -- serve {{args}}
