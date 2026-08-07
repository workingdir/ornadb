default: check

# Run the full local quality gate used by continuous integration.
check: fmt build lint test

# Verify formatting without changing source files.
fmt:
    cargo fmt --all -- --check

# Compile every workspace target.
build:
    cargo check --workspace --all-targets

# Reject all Clippy warnings across workspace targets.
lint:
    cargo clippy --workspace --all-targets -- -D warnings

# Run the complete workspace test suite.
test:
    cargo test --workspace --all-targets
