use std::{env, error::Error, io, path::PathBuf, process};

use orna_client::{
    RuntimeLibrary, RuntimeSession, RuntimeUiBatch, RuntimeUiOperation, RuntimeValueInput,
};

const WINDOW_NODE: u64 = 100;
const PANEL_NODE: u64 = 101;
const TEXT_NODE: u64 = 102;
const BUTTON_NODE: u64 = 103;
const BUTTON_ACTION: u64 = 200;

fn main() {
    if let Err(error) = run() {
        eprintln!("runtime_qt_smoke: {error}");
        process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let library = RuntimeLibrary::load_qt(runtime_path()?)?;
    let mut session = RuntimeSession::new_qt(library, "en-GB", "UTC", "light")?;
    let surface = session.create_surface("OrnaDB Rust Qt smoke")?;

    let mut batch = RuntimeUiBatch::new(1);
    batch.push(RuntimeUiOperation::mount_node(
        WINDOW_NODE,
        0,
        "root",
        0,
        "std.ui.window",
        1,
        0,
        RuntimeValueInput::empty(),
    ))?;
    batch.push(RuntimeUiOperation::mount_node(
        PANEL_NODE,
        WINDOW_NODE,
        "content",
        0,
        "std.ui.panel",
        1,
        0,
        RuntimeValueInput::empty(),
    ))?;
    batch.push(RuntimeUiOperation::mount_node(
        TEXT_NODE,
        PANEL_NODE,
        "content",
        0,
        "std.ui.text",
        1,
        0,
        RuntimeValueInput::empty(),
    ))?;
    batch.push(RuntimeUiOperation::mount_node(
        BUTTON_NODE,
        PANEL_NODE,
        "content",
        1,
        "std.ui.button",
        1,
        0,
        RuntimeValueInput::empty(),
    ))?;
    batch.push(RuntimeUiOperation::set_property(
        WINDOW_NODE,
        "title",
        text_value("OrnaDB Rust Qt smoke"),
    ))?;
    batch.push(RuntimeUiOperation::set_property(
        TEXT_NODE,
        "text",
        text_value("ABI-backed Rust Qt runtime"),
    ))?;
    batch.push(RuntimeUiOperation::set_property(
        BUTTON_NODE,
        "label",
        text_value("Continue"),
    ))?;
    batch.push(RuntimeUiOperation::set_property(
        BUTTON_NODE,
        "enabled",
        RuntimeValueInput::new(0, "std.boolean", vec![1]),
    ))?;
    batch.push(RuntimeUiOperation::bind_action(
        BUTTON_NODE,
        "clicked",
        BUTTON_ACTION,
        "std.text",
    ))?;

    session.apply_batch(surface, &batch)?;
    let canonical_state = session.capture_semantic_state(surface)?;
    if canonical_state.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "runtime returned an empty canonical semantic state",
        )
        .into());
    }

    session.destroy_surface(surface)?;
    session.poll(1)?;
    session.shutdown()?;
    println!(
        "runtime_qt_smoke: canonical state {} bytes",
        canonical_state.len()
    );
    Ok(())
}

fn runtime_path() -> Result<PathBuf, io::Error> {
    let mut args = env::args_os();
    let _program = args.next();
    let path = args.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: runtime_qt_smoke <runtime-shared-library>",
        )
    })?;
    if args.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: runtime_qt_smoke <runtime-shared-library>",
        ));
    }
    Ok(path.into())
}

fn text_value(value: &str) -> RuntimeValueInput {
    RuntimeValueInput::new(0, "std.text", value.as_bytes().to_vec())
}
