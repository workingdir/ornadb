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

# Start the private PostgreSQL development kernel.
postgres-up:
    docker compose up --detach postgres

# Stop PostgreSQL without deleting its persistent volume.
postgres-stop:
    docker compose stop postgres

# Show PostgreSQL container status.
postgres-status:
    docker compose ps postgres

# Verify that PostgreSQL accepts authenticated connections.
postgres-health:
    docker compose exec postgres pg_isready --username=ornadb_dev --dbname=ornadb_dev

# Open an operator shell in the private PostgreSQL kernel.
backend-shell:
    docker compose exec postgres psql --username=ornadb_dev --dbname=ornadb_dev
