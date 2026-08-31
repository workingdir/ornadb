default: check

# Run the default local fmt/build/lint/non-ignored test gate; CI also runs separate editor-tooling and Compose kernel gates.
check: fmt build lint test

# Check the SQLite adapter, storage contract, and local CLI binary offline.
sqlite-check:
    cargo check --locked --offline -p orna-storage -p orna-sqlite
    cargo check --locked --offline -p orna-server --bin orna

# Run the deterministic SQLite adapter and process-boundary smoke tests offline.
sqlite-smoke:
    cargo test --locked --offline -p orna-sqlite --lib
    cargo run --locked --offline -p orna-sqlite --example revision_store_smoke
    cargo test --locked --offline -p orna-server --test sqlite_backend -- --nocapture

# Verify formatting without changing source files.
fmt:
    cargo fmt --all -- --check


# Run the accepted TTY renderer demo for terminal documents and byte streams.
runtime-tty-demo:
    cargo run --locked --offline -p orna-runtime-tty --example runtime_demo


# Build the binary, start a temporary local server, and invoke std.invoke.echo.
local-cli-demo:
    bash scripts/local-cli-demo.sh


# Exercise CLIENT artifact kind and payload-digest validation.
client-artifact-demo:
    cargo run --locked --offline -p orna-client --example client_artifact_demo


# Exercise component-boundary matching for a local filesystem grant.
client-capability-demo:
    cargo run --locked --offline -p orna-client --example client_capability_demo

# Run the accepted offline demo registry and standalone local demos.
demo-suite: demo-check runtime-tty-demo client-artifact-demo client-capability-demo

# Build the first production Qt runtime against the canonical ABI header.
runtime-qt-build:
    cmake -S runtimes/qt -B target/runtime-qt
    cmake --build target/runtime-qt --parallel


# Build and run the Qt runtime demo against a real display.
runtime-qt-demo: runtime-qt-build
    test -n "${DISPLAY-}${WAYLAND_DISPLAY-}" || (echo "runtime-qt-demo: DISPLAY or WAYLAND_DISPLAY is required" >&2; exit 2)
    env -u QT_QPA_PLATFORM target/runtime-qt/orna-runtime-qt-demo

# Build the Qt runtime and run the static Studio shell smoke.
studio-qt-demo: runtime-qt-build
    just studio-qt-smoke target/runtime-qt/liborna-runtime-qt.so

# Run the Qt runtime contract smoke test with an offscreen platform.
runtime-qt-test:
    cmake -S runtimes/qt -B target/runtime-qt
    cmake --build target/runtime-qt --parallel
    ctest --test-dir target/runtime-qt --output-on-failure



# Run the Rust Qt runtime loader/session smoke test against an explicit shared library path.
runtime-qt-rust-smoke runtime_path:
    QT_QPA_PLATFORM=offscreen cargo run --locked --offline -p orna-client --example runtime_qt_smoke -- {{runtime_path}}


# Prove adapter shutdown drains a full callback queue before native shutdown.
runtime-qt-shutdown-queue-smoke runtime_path:
    QT_QPA_PLATFORM=offscreen cargo run --locked --offline -p orna-client --example runtime_qt_shutdown_queue_smoke -- {{runtime_path}}

# Run the Studio shell demo once against an explicit Qt runtime path.
studio-qt-smoke runtime_path:
    QT_QPA_PLATFORM=offscreen cargo run --locked --offline -p orna-client --example studio_demo -- {{runtime_path}} --smoke

# Run the Rust Studio shell interactively against a display server.
studio-qt-display: runtime-qt-build
    test -n "${DISPLAY-}${WAYLAND_DISPLAY-}" || (echo "studio-qt-display: DISPLAY or WAYLAND_DISPLAY is required" >&2; exit 2)
    env -u QT_QPA_PLATFORM cargo run --locked -p orna-client --example studio_demo -- target/runtime-qt/liborna-runtime-qt.so

# Run the Rust Studio shell once against a display server and exit.
studio-qt-display-smoke: runtime-qt-build
    test -n "${DISPLAY-}${WAYLAND_DISPLAY-}" || (echo "studio-qt-display-smoke: DISPLAY or WAYLAND_DISPLAY is required" >&2; exit 2)
    env -u QT_QPA_PLATFORM cargo run --locked -p orna-client --example studio_demo -- target/runtime-qt/liborna-runtime-qt.so --smoke

# Emit one registered Studio action and verify its feedback update.
studio-qt-action-smoke: runtime-qt-build
    test -n "${DISPLAY-}${WAYLAND_DISPLAY-}" || (echo "studio-qt-action-smoke: DISPLAY or WAYLAND_DISPLAY is required" >&2; exit 2)
    env -u QT_QPA_PLATFORM cargo run --locked -p orna-client --example studio_demo -- target/runtime-qt/liborna-runtime-qt.so --smoke-action

# Exercise the accepted TTY and Qt runtime smoke paths without a display server.
runtime-suite: runtime-qt-test
    cargo run --locked --offline -p orna-runtime-tty --example runtime_demo > target/runtime-tty-demo-output.bin
    QT_QPA_PLATFORM=offscreen target/runtime-qt/orna-runtime-qt-demo --smoke

# Exercise the Qt visual and action paths against a display server.
runtime-display-suite: runtime-qt-build
    test -n "${DISPLAY-}${WAYLAND_DISPLAY-}" || (echo "runtime-display-suite: DISPLAY or WAYLAND_DISPLAY is required" >&2; exit 2)
    env -u QT_QPA_PLATFORM target/runtime-qt/orna-runtime-qt-visual target/runtime-qt/orna-runtime-qt-display.png
    env -u QT_QPA_PLATFORM target/runtime-qt/orna-runtime-qt-demo --smoke


# Validate the accepted headless runtime C-shaped ABI header against the canonical spec bundle.
# The canonical header is an external sibling input in this checkout; clean CI hosts without
# ../spec cannot run this local gate until the checkout contract is resolved.
runtime-abi-header-check:
    gcc -std=c11 -fsyntax-only ../spec/spec/orna_runtime_abi_v1.h

# Compile the Linux x86_64 C assertions against the canonical header and Rust mirror values.
runtime-abi-parity:
    gcc -std=c11 -fno-short-enums -Wall -Wextra -Werror -I../spec -fsyntax-only crates/orna-client/tests/runtime_abi_parity.c

# Compile every workspace target.
build:
    cargo check --locked --workspace --all-targets

# Reject all Clippy warnings across workspace targets.
lint:
    cargo clippy --locked --workspace --all-targets -- -D warnings

# Run workspace tests excluding #[ignore] tests.
test:
    cargo test --locked --workspace --all-targets

# Validate the tree-sitter grammar and editor metadata without installing editor runtimes.
# This static gate requires its CLI prerequisites: Python 3.11+, tree-sitter CLI, node, and cargo.
editor-tooling-check:
    python3 scripts/check-editor-tooling.py

# Run every runnable accepted source-check/offline demo in manifest order.
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
    cargo test --locked --package orna-server --features test-hooks --test standard_database installed_resource_socket_delivers_values_and_enforces_windows_and_grants -- --ignored --exact --test-threads=1

# Run every ignored PostgreSQL integration test against an isolated database.
kernel-test:
    #!/usr/bin/env bash
    set -euo pipefail
    kernel_database="ornadb_kernel_gate_${BASHPID}_$(date -u +%s%N)"
    cleanup() {
        local status=$?
        trap - EXIT INT TERM
        docker compose exec -T postgres dropdb --if-exists --force --username=ornadb_dev "$kernel_database" || true
        docker compose stop postgres || true
        exit "$status"
    }
    trap cleanup EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
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
    cargo test --locked --package orna-server --features test-hooks "${server_tests[@]}" -- --ignored --test-threads=1
    docker compose exec -T postgres dropdb --if-exists --force --username=ornadb_dev "$kernel_database"
    docker compose exec -T postgres createdb --template=template0 --username=ornadb_dev "$kernel_database"
    export ORNA_TEST_POSTGRES_URL="host=127.0.0.1 port=55432 user=ornadb_dev password=ornadb_dev_password dbname=$kernel_database"
    cargo test --locked --package orna-postgres --features test-hooks --lib -- --ignored --test-threads=1
    cargo test --locked --package orna-postgres --features test-hooks "${postgres_tests[@]}" -- --ignored --test-threads=1
