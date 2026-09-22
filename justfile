default: check

# Run the default local fmt/build/lint/non-ignored test/rustdoc gate.
check: fmt build lint test rustdoc-check


# Verify formatting without changing source files.
fmt:
    cargo fmt --all -- --check


# Build all workspace API documentation and reject rustdoc warnings.
rustdoc-check:
    RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --all-features --no-deps

# Run the accepted TTY renderer demo for terminal documents and byte streams.
runtime-tty-demo:
    cargo run --locked --offline -p orna-runtime-tty --example runtime_demo




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
runtime-qt-demo:
    test -n "${DISPLAY-}${WAYLAND_DISPLAY-}" || (echo "runtime-qt-demo: DISPLAY or WAYLAND_DISPLAY is required" >&2; exit 2)
    just runtime-qt-build
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
studio-qt-display:
    test -n "${DISPLAY-}${WAYLAND_DISPLAY-}" || (echo "studio-qt-display: DISPLAY or WAYLAND_DISPLAY is required" >&2; exit 2)
    just runtime-qt-build
    env -u QT_QPA_PLATFORM cargo run --locked -p orna-client --example studio_demo -- target/runtime-qt/liborna-runtime-qt.so

# Run the Rust Studio shell once against a display server and exit.
studio-qt-display-smoke:
    test -n "${DISPLAY-}${WAYLAND_DISPLAY-}" || (echo "studio-qt-display-smoke: DISPLAY or WAYLAND_DISPLAY is required" >&2; exit 2)
    just runtime-qt-build
    env -u QT_QPA_PLATFORM cargo run --locked -p orna-client --example studio_demo -- target/runtime-qt/liborna-runtime-qt.so --smoke

# Emit one registered Studio action and verify its feedback update.
studio-qt-action-smoke:
    test -n "${DISPLAY-}${WAYLAND_DISPLAY-}" || (echo "studio-qt-action-smoke: DISPLAY or WAYLAND_DISPLAY is required" >&2; exit 2)
    just runtime-qt-build
    env -u QT_QPA_PLATFORM cargo run --locked -p orna-client --example studio_demo -- target/runtime-qt/liborna-runtime-qt.so --smoke-action

# Exercise the accepted TTY and Qt runtime smoke paths without a display server.
runtime-suite: runtime-qt-test
    cargo run --locked --offline -p orna-runtime-tty --example runtime_demo > target/runtime-tty-demo-output.bin
    QT_QPA_PLATFORM=offscreen target/runtime-qt/orna-runtime-qt-demo --smoke

# Exercise the Qt visual and action paths against a display server.
runtime-display-suite:
    test -n "${DISPLAY-}${WAYLAND_DISPLAY-}" || (echo "runtime-display-suite: DISPLAY or WAYLAND_DISPLAY is required" >&2; exit 2)
    just runtime-qt-build
    env -u QT_QPA_PLATFORM target/runtime-qt/orna-runtime-qt-visual target/runtime-qt/orna-runtime-qt-display.png
    env -u QT_QPA_PLATFORM target/runtime-qt/orna-runtime-qt-demo --smoke


# Validate the accepted headless runtime C-shaped ABI header against the canonical spec bundle.
# The canonical header is an external sibling input in this checkout; hosts without
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
