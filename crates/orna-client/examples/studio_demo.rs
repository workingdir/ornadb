use std::{env, error::Error, io, path::PathBuf, process};

use orna_client::{
    RuntimeEventSnapshot, RuntimeLibrary, RuntimeSession, RuntimeSessionError, RuntimeUiBatch,
    RuntimeUiOperation, RuntimeValueInput,
};

// Stable aliases keep the semantic tree and action bindings reproducible across runs.
const WINDOW_NODE: u64 = 100;
const ROOT_COLUMN_NODE: u64 = 101;
const HEADER_ROW_NODE: u64 = 102;
const HEADER_TEXT_NODE: u64 = 103;
const HEADER_SUBTITLE_NODE: u64 = 104;
const MAIN_ROW_NODE: u64 = 105;
const CONNECTIONS_PANEL_NODE: u64 = 106;
const CONNECTIONS_COLUMN_NODE: u64 = 107;
const CONNECTIONS_HEADING_NODE: u64 = 108;
const CONNECTION_INPUT_NODE: u64 = 109;
const SELECT_CONNECTION_BUTTON_NODE: u64 = 110;
const EDITOR_PANEL_NODE: u64 = 111;
const EDITOR_COLUMN_NODE: u64 = 112;
const EDITOR_HEADING_NODE: u64 = 113;
const EDITOR_INPUT_NODE: u64 = 114;
const EDITOR_ACTION_ROW_NODE: u64 = 115;
const RUN_BUTTON_NODE: u64 = 116;
const CLEAR_BUTTON_NODE: u64 = 117;
const RESULTS_PANEL_NODE: u64 = 118;
const RESULTS_COLUMN_NODE: u64 = 119;
const RESULTS_HEADING_NODE: u64 = 120;
const RESULTS_BODY_NODE: u64 = 121;
const STATUS_ROW_NODE: u64 = 122;
const STATUS_TEXT_NODE: u64 = 123;

const SELECT_CONNECTION_ACTION: u64 = 200;
const RUN_ACTION: u64 = 201;
const CLEAR_ACTION: u64 = 202;
const STUDIO_TITLE: &str = "Orna Studio - Qt shell";

struct Arguments {
    runtime_path: PathBuf,
    smoke: bool,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("studio_demo: {error}");
        process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let arguments = parse_arguments()?;
    println!(
        "studio_demo: loading Qt runtime {}",
        arguments.runtime_path.display()
    );

    let library = RuntimeLibrary::load_qt(&arguments.runtime_path)?;
    let mut session = RuntimeSession::new_qt(library, "en-GB", "UTC", "light")?;
    let surface = session.create_surface(STUDIO_TITLE)?;
    println!("studio_demo: created surface {surface}");

    let batch = studio_batch()?;
    session.apply_batch(surface, &batch)?;
    println!(
        "studio_demo: applied {} UI operations",
        batch.operations.len()
    );

    let canonical_state = session.capture_semantic_state(surface)?;
    if canonical_state.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "runtime returned an empty canonical semantic state",
        )
        .into());
    }
    println!(
        "studio_demo: captured canonical state ({} bytes)",
        canonical_state.len()
    );

    let surface_closed = if arguments.smoke {
        // Smoke mode deliberately performs exactly one caller-owned event-loop poll.
        session.poll(1)?;
        let closed = has_surface_closed_event(session.drain_events(), surface);
        println!("studio_demo: smoke poll complete");
        closed
    } else {
        session.set_surface_visible(surface, true)?;
        println!("studio_demo: surface visible; waiting for close");
        let mut closed = false;
        while !closed {
            session.poll(50)?;
            closed = has_surface_closed_event(session.drain_events(), surface);
        }
        println!("studio_demo: received RuntimeSurfaceClosed");
        closed
    };

    // A native close is already reaped by the provider's poll.  Otherwise close
    // the still-live surface before requesting terminal runtime shutdown.
    if !surface_closed {
        session.destroy_surface(surface)?;
    }
    session.shutdown()?;
    println!("studio_demo: shutdown complete");
    Ok(())
}

fn parse_arguments() -> Result<Arguments, io::Error> {
    let mut runtime_path = None;
    let mut smoke = false;

    for argument in env::args_os().skip(1) {
        if argument == "--smoke" {
            if smoke {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "duplicate --smoke flag; usage: studio_demo <runtime-shared-library> [--smoke]",
                ));
            }
            smoke = true;
            continue;
        }

        if argument
            .to_str()
            .is_some_and(|value| value.starts_with('-'))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "unknown argument `{}`; usage: studio_demo <runtime-shared-library> [--smoke]",
                    argument.to_string_lossy()
                ),
            ));
        }

        if runtime_path.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected one runtime path and optional --smoke; usage: studio_demo <runtime-shared-library> [--smoke]",
            ));
        }
        runtime_path = Some(PathBuf::from(argument));
    }

    let runtime_path = runtime_path.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "missing runtime path; usage: studio_demo <runtime-shared-library> [--smoke]",
        )
    })?;

    Ok(Arguments {
        runtime_path,
        smoke,
    })
}

fn studio_batch() -> Result<RuntimeUiBatch, RuntimeSessionError> {
    let mut batch = RuntimeUiBatch::new(1);

    mount(&mut batch, WINDOW_NODE, 0, "root", 0, "std.ui.window")?;
    mount(
        &mut batch,
        ROOT_COLUMN_NODE,
        WINDOW_NODE,
        "content",
        0,
        "std.ui.column",
    )?;
    mount(
        &mut batch,
        HEADER_ROW_NODE,
        ROOT_COLUMN_NODE,
        "content",
        0,
        "std.ui.row",
    )?;
    mount(
        &mut batch,
        HEADER_TEXT_NODE,
        HEADER_ROW_NODE,
        "content",
        0,
        "std.ui.text",
    )?;
    mount(
        &mut batch,
        HEADER_SUBTITLE_NODE,
        HEADER_ROW_NODE,
        "content",
        1,
        "std.ui.text",
    )?;
    mount(
        &mut batch,
        MAIN_ROW_NODE,
        ROOT_COLUMN_NODE,
        "content",
        1,
        "std.ui.row",
    )?;
    mount(
        &mut batch,
        CONNECTIONS_PANEL_NODE,
        MAIN_ROW_NODE,
        "content",
        0,
        "std.ui.panel",
    )?;
    mount(
        &mut batch,
        CONNECTIONS_COLUMN_NODE,
        CONNECTIONS_PANEL_NODE,
        "content",
        0,
        "std.ui.column",
    )?;
    mount(
        &mut batch,
        CONNECTIONS_HEADING_NODE,
        CONNECTIONS_COLUMN_NODE,
        "content",
        0,
        "std.ui.text",
    )?;
    mount(
        &mut batch,
        CONNECTION_INPUT_NODE,
        CONNECTIONS_COLUMN_NODE,
        "content",
        1,
        "std.ui.text_input",
    )?;
    mount(
        &mut batch,
        SELECT_CONNECTION_BUTTON_NODE,
        CONNECTIONS_COLUMN_NODE,
        "content",
        2,
        "std.ui.button",
    )?;
    mount(
        &mut batch,
        EDITOR_PANEL_NODE,
        MAIN_ROW_NODE,
        "content",
        1,
        "std.ui.panel",
    )?;
    mount(
        &mut batch,
        EDITOR_COLUMN_NODE,
        EDITOR_PANEL_NODE,
        "content",
        0,
        "std.ui.column",
    )?;
    mount(
        &mut batch,
        EDITOR_HEADING_NODE,
        EDITOR_COLUMN_NODE,
        "content",
        0,
        "std.ui.text",
    )?;
    mount(
        &mut batch,
        EDITOR_INPUT_NODE,
        EDITOR_COLUMN_NODE,
        "content",
        1,
        "std.ui.text_input",
    )?;
    mount(
        &mut batch,
        EDITOR_ACTION_ROW_NODE,
        EDITOR_COLUMN_NODE,
        "content",
        2,
        "std.ui.row",
    )?;
    mount(
        &mut batch,
        RUN_BUTTON_NODE,
        EDITOR_ACTION_ROW_NODE,
        "content",
        0,
        "std.ui.button",
    )?;
    mount(
        &mut batch,
        CLEAR_BUTTON_NODE,
        EDITOR_ACTION_ROW_NODE,
        "content",
        1,
        "std.ui.button",
    )?;
    mount(
        &mut batch,
        RESULTS_PANEL_NODE,
        MAIN_ROW_NODE,
        "content",
        2,
        "std.ui.panel",
    )?;
    mount(
        &mut batch,
        RESULTS_COLUMN_NODE,
        RESULTS_PANEL_NODE,
        "content",
        0,
        "std.ui.column",
    )?;
    mount(
        &mut batch,
        RESULTS_HEADING_NODE,
        RESULTS_COLUMN_NODE,
        "content",
        0,
        "std.ui.text",
    )?;
    mount(
        &mut batch,
        RESULTS_BODY_NODE,
        RESULTS_COLUMN_NODE,
        "content",
        1,
        "std.ui.text",
    )?;
    mount(
        &mut batch,
        STATUS_ROW_NODE,
        ROOT_COLUMN_NODE,
        "content",
        2,
        "std.ui.row",
    )?;
    mount(
        &mut batch,
        STATUS_TEXT_NODE,
        STATUS_ROW_NODE,
        "content",
        0,
        "std.ui.text",
    )?;

    set_text(&mut batch, WINDOW_NODE, "title", STUDIO_TITLE)?;
    set_text(&mut batch, HEADER_TEXT_NODE, "text", "Orna Studio")?;
    set_text(
        &mut batch,
        HEADER_SUBTITLE_NODE,
        "text",
        "Client-side shell",
    )?;
    set_text(&mut batch, CONNECTIONS_HEADING_NODE, "text", "Connections")?;
    set_text(&mut batch, CONNECTION_INPUT_NODE, "text", "Local runtime")?;
    set_text(
        &mut batch,
        CONNECTION_INPUT_NODE,
        "placeholder",
        "Connection name",
    )?;
    set_enabled(&mut batch, CONNECTION_INPUT_NODE, true)?;
    set_text(&mut batch, SELECT_CONNECTION_BUTTON_NODE, "label", "Select")?;
    set_enabled(&mut batch, SELECT_CONNECTION_BUTTON_NODE, true)?;
    set_text(&mut batch, EDITOR_HEADING_NODE, "text", "Editor")?;
    set_text(&mut batch, EDITOR_INPUT_NODE, "text", "Type here")?;
    set_text(
        &mut batch,
        EDITOR_INPUT_NODE,
        "placeholder",
        "Editor buffer",
    )?;
    set_enabled(&mut batch, EDITOR_INPUT_NODE, true)?;
    set_text(&mut batch, RUN_BUTTON_NODE, "label", "Run")?;
    set_enabled(&mut batch, RUN_BUTTON_NODE, true)?;
    set_text(&mut batch, CLEAR_BUTTON_NODE, "label", "Clear")?;
    set_enabled(&mut batch, CLEAR_BUTTON_NODE, true)?;
    set_text(&mut batch, RESULTS_HEADING_NODE, "text", "Results")?;
    set_text(&mut batch, RESULTS_BODY_NODE, "text", "No results yet")?;
    set_text(&mut batch, STATUS_TEXT_NODE, "text", "Ready - local shell")?;

    bind_action(
        &mut batch,
        SELECT_CONNECTION_BUTTON_NODE,
        SELECT_CONNECTION_ACTION,
    )?;
    bind_action(&mut batch, RUN_BUTTON_NODE, RUN_ACTION)?;
    bind_action(&mut batch, CLEAR_BUTTON_NODE, CLEAR_ACTION)?;

    Ok(batch)
}

fn mount(
    batch: &mut RuntimeUiBatch,
    node: u64,
    parent: u64,
    slot: &str,
    ordinal: usize,
    contract: &str,
) -> Result<(), RuntimeSessionError> {
    batch.push(RuntimeUiOperation::mount_node(
        node,
        parent,
        slot,
        ordinal,
        contract,
        1,
        0,
        RuntimeValueInput::empty(),
    ))
}

fn set_text(
    batch: &mut RuntimeUiBatch,
    node: u64,
    property: &str,
    value: &str,
) -> Result<(), RuntimeSessionError> {
    batch.push(RuntimeUiOperation::set_property(
        node,
        property,
        RuntimeValueInput::new(0, "std.text", value.as_bytes().to_vec()),
    ))
}

fn set_enabled(
    batch: &mut RuntimeUiBatch,
    node: u64,
    enabled: bool,
) -> Result<(), RuntimeSessionError> {
    batch.push(RuntimeUiOperation::set_property(
        node,
        "enabled",
        RuntimeValueInput::new(0, "std.boolean", vec![u8::from(enabled)]),
    ))
}

fn bind_action(
    batch: &mut RuntimeUiBatch,
    node: u64,
    action: u64,
) -> Result<(), RuntimeSessionError> {
    batch.push(RuntimeUiOperation::bind_action(
        node, "clicked", action, "std.text",
    ))
}

fn has_surface_closed_event(events: Vec<RuntimeEventSnapshot>, surface: u64) -> bool {
    events.into_iter().any(|event| {
        matches!(
            event,
            RuntimeEventSnapshot::SurfaceClosed(closed) if closed.surface == surface
        )
    })
}
