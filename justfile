default: check

# Run the default local fmt/build/lint/non-ignored test gate; CI also runs separate editor-tooling and Compose kernel gates.
check: fmt build lint test

# Verify formatting without changing source files.
fmt:
    cargo fmt --all -- --check


# Run the accepted TTY renderer demo for terminal documents and byte streams.
runtime-tty-demo:
    cargo run -p orna-runtime-tty --example runtime_demo


# Build the binary, start a temporary local server, and invoke std.invoke.echo.
local-cli-demo:
    bash scripts/local-cli-demo.sh

# Build the first production Qt runtime against the canonical ABI header.
runtime-qt-build:
    cmake -S runtimes/qt -B target/runtime-qt
    cmake --build target/runtime-qt --parallel

# Run the Qt runtime contract smoke test with an offscreen platform.
runtime-qt-test:
    cmake -S runtimes/qt -B target/runtime-qt
    cmake --build target/runtime-qt --parallel
    ctest --test-dir target/runtime-qt --output-on-failure

# Build the separate Debian package for the fixed Qt runtime path.
runtime-qt-package:
    cmake -S runtimes/qt -B target/runtime-qt -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/usr
    cmake --build target/runtime-qt --parallel
    cpack --config target/runtime-qt/CPackConfig.cmake -G DEB

# Run the Rust Qt runtime loader/session smoke test against an explicit shared library path.
runtime-qt-rust-smoke runtime_path:
    QT_QPA_PLATFORM=offscreen cargo run -p orna-client --example runtime_qt_smoke -- {{runtime_path}}

# Run the Studio shell demo once against an explicit Qt runtime path.
studio-qt-smoke runtime_path:
    QT_QPA_PLATFORM=offscreen cargo run -p orna-client --example studio_demo -- {{runtime_path}} --smoke

# Validate the accepted headless runtime C-shaped ABI header against the canonical spec bundle.
# The canonical header is an external sibling input in this checkout; clean CI hosts without
# ../spec cannot run this local gate until the packaging/checkout contract is resolved.
runtime-abi-header-check:
    gcc -std=c11 -fsyntax-only ../spec/spec/orna_runtime_abi_v1.h

# Compile the Linux x86_64 C assertions against the canonical header and Rust mirror values.
runtime-abi-parity:
    gcc -std=c11 -fno-short-enums -Wall -Wextra -Werror -I../spec -fsyntax-only crates/orna-client/tests/runtime_abi_parity.c

# Compile every workspace target.
build:
    cargo check --workspace --all-targets

# Reject all Clippy warnings across workspace targets.
lint:
    cargo clippy --workspace --all-targets -- -D warnings

# Run workspace tests excluding #[ignore] tests.
test:
    cargo test --workspace --all-targets

# Validate the tree-sitter grammar and editor metadata without installing editor runtimes.
# This static gate requires its CLI prerequisites: Python 3.11+, tree-sitter CLI, node, and cargo.
editor-tooling-check:
    python3 scripts/check-editor-tooling.py

# Run every accepted offline source-check demo in manifest order.
demo-check:
    python3 scripts/run-demos.py

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
    kernel_database=ornadb_kernel_gate
    cleanup() {
        docker compose exec -T postgres dropdb --if-exists --username=ornadb_dev "$kernel_database" || true
        docker compose stop postgres || true
    }
    trap cleanup EXIT
    docker compose up --detach --wait postgres
    export ORNA_TEST_POSTGRES_ADMIN_URL="host=127.0.0.1 port=55432 user=ornadb_dev password=ornadb_dev_password"
    export ORNA_TEST_POSTGRES_URL="host=127.0.0.1 port=55432 user=ornadb_dev password=ornadb_dev_password dbname=ornadb_dev"
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
    docker compose exec -T postgres dropdb --if-exists --username=ornadb_dev "$kernel_database"
    docker compose exec -T postgres createdb --username=ornadb_dev "$kernel_database"
    export ORNA_TEST_POSTGRES_URL="host=127.0.0.1 port=55432 user=ornadb_dev password=ornadb_dev_password dbname=$kernel_database"
    cargo test --package orna-postgres --features test-hooks --lib -- --ignored --test-threads=1
    cargo test --package orna-postgres --features test-hooks "${postgres_tests[@]}" -- --ignored --test-threads=1
