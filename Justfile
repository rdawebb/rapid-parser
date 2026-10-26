set working-directory := "rapid-parser"

# Format Rust code
format:
    cargo fmt

# Lint with clippy
lint:
    cargo clippy --all-targets --all-features -- -D warnings

# Check Rust code
check: format lint
    cargo check

# Build Rust code
build:
    cargo build

# Release build Rust code
build-r:
    cargo build --release

# Run all tests
test:
    cargo test
