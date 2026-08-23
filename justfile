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

# Validate the tree-sitter grammar and editor metadata without installing tools.
editor-tooling-check:
    python3 scripts/check-editor-tooling.py

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

# Run only the installed resource transport durability proof.
kernel-resource-audit-proof:
    #!/usr/bin/env bash
    set -euo pipefail
    cleanup() {
        docker compose stop postgres || true
    }
    trap cleanup EXIT
    docker compose up --detach --wait postgres
    export ORNA_TEST_POSTGRES_ADMIN_URL='host=127.0.0.1 port=55432 user=ornadb_dev password=ornadb_dev_password'
    export ORNA_TEST_POSTGRES_URL='host=127.0.0.1 port=55432 user=ornadb_dev password=ornadb_dev_password dbname=ornadb_dev'
    cargo test --package orna-server --features test-hooks --test standard_database installed_resource_socket_delivers_values_and_enforces_windows_and_grants -- --ignored --exact --test-threads=1

# Run every ignored PostgreSQL integration test against an isolated database.
kernel-test:
    #!/usr/bin/env bash
    set -euo pipefail
    cleanup() {
        docker compose stop postgres || true
    }
    trap cleanup EXIT
    docker compose up --detach --wait postgres
    export ORNA_TEST_POSTGRES_ADMIN_URL='host=127.0.0.1 port=55432 user=ornadb_dev password=ornadb_dev_password'
    export ORNA_TEST_POSTGRES_URL='host=127.0.0.1 port=55432 user=ornadb_dev password=ornadb_dev_password dbname=ornadb_dev'
    server_tests=()
    for test_path in crates/orna-server/tests/*.rs; do
        test_name=${test_path##*/}
        server_tests+=(--test "${test_name%.rs}")
    done
    postgres_tests=()
    for test_path in crates/orna-postgres/tests/*.rs; do
        test_name=${test_path##*/}
        postgres_tests+=(--test "${test_name%.rs}")
    done
    cargo test --package orna-server --features test-hooks "${server_tests[@]}" -- --ignored --test-threads=1
    cargo test --package orna-postgres --features test-hooks --lib -- --ignored --test-threads=1
    cargo test --package orna-postgres --features test-hooks "${postgres_tests[@]}" -- --ignored --test-threads=1
