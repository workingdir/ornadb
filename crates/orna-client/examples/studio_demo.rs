use std::{env, error::Error, io, path::PathBuf, process};

use orna_client::{
    AbiSurfaceHandle, QtRuntimeExecutor, RuntimeEventSnapshot, RuntimeLibrary, RuntimeSession,
};
use orna_standard::UI_MAGIC;
use serde_json::{Value, json};

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
    let session = RuntimeSession::new_qt(library, "en-GB", "UTC", "light")?;
    let mut host = QtRuntimeExecutor::new(session);
    let frame = studio_ui_frame("Source and invocation workspace")?;
    let surface = host.show_window(STUDIO_TITLE, &frame)?;
    println!("studio_demo: displayed canonical UI through shared Qt host");
    let refreshed_frame = studio_ui_frame("Source and invocation workspace (refreshed)")?;
    host.update_window(surface, STUDIO_TITLE, &refreshed_frame)?;
    println!("studio_demo: applied canonical UI refresh through shared Qt host");

    let canonical_state = host.capture_semantic_state(surface)?;
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
        host.poll_runtime(1)?;
        let events = host.drain_runtime_events();
        let closed = consume_runtime_events(&host, events, surface);
        println!("studio_demo: smoke poll complete");
        closed
    } else {
        println!("studio_demo: surface visible; waiting for close");
        let mut closed = false;
        while !closed {
            host.poll_runtime(50)?;
            let events = host.drain_runtime_events();
            closed = consume_runtime_events(&host, events, surface);
        }
        println!("studio_demo: received RuntimeSurfaceClosed");
        closed
    };

    if !surface_closed {
        host.destroy_surface(surface)?;
    }
    host.shutdown()?;
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
                    "duplicate --smoke; usage: studio_demo <runtime-shared-library> [--smoke]",
                ));
            }
            smoke = true;
            continue;
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

fn studio_ui_frame(workspace_label: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    let body = ui_node(
        "std.ui.column",
        json!({}),
        vec![
            text_node("Orna Studio"),
            panel_node(vec![text_node(workspace_label)]),
            ui_node(
                "std.ui.text_input",
                json!({
                    "placeholder": {"type": "std.types.text", "value": "Search functions"},
                    "text": {"type": "std.types.text", "value": ""},
                    "enabled": {"type": "std.types.boolean", "value": true}
                }),
                Vec::new(),
                json!({}),
            ),
            button_node("Run", "studio.run"),
            ui_node(
                "std.ui.tabs",
                json!({}),
                vec![text_node("Results")],
                json!({}),
            ),
        ],
        json!({}),
    );
    let body = serde_json::to_vec(&body)?;
    let body_length = u32::try_from(body.len())?;
    let mut frame = UI_MAGIC.as_bytes().to_vec();
    frame.extend_from_slice(&body_length.to_be_bytes());
    frame.extend_from_slice(&body);
    Ok(frame)
}

fn text_node(text: &str) -> Value {
    ui_node(
        "std.ui.text",
        json!({"text": {"type": "std.types.text", "value": text}}),
        Vec::new(),
        json!({}),
    )
}

fn panel_node(children: Vec<Value>) -> Value {
    ui_node("std.ui.panel", json!({}), children, json!({}))
}

fn button_node(label: &str, action_id: &str) -> Value {
    ui_node(
        "std.ui.button",
        json!({"label": {"type": "std.types.text", "value": label}}),
        Vec::new(),
        json!({
            "clicked": {
                "action_id": action_id,
                "input_type": "std.text",
                "trace": true
            }
        }),
    )
}

fn ui_node(contract: &str, properties: Value, children: Vec<Value>, actions: Value) -> Value {
    let slots = if children.is_empty() {
        json!({})
    } else {
        json!({"content": children})
    };
    json!({
        "kind": "node",
        "contract": {"id": contract, "name": contract, "version": "1.0"},
        "properties": properties,
        "slots": slots,
        "actions": actions
    })
}

fn consume_runtime_events(
    host: &QtRuntimeExecutor,
    events: Vec<RuntimeEventSnapshot>,
    surface: AbiSurfaceHandle,
) -> bool {
    let mut surface_closed = false;
    for event in events {
        match event {
            RuntimeEventSnapshot::Action(action) if action.surface == surface => {
                if let Some(binding) = host.action_binding(action.action) {
                    println!("studio_demo: action requested {}", binding.action_id());
                } else {
                    println!(
                        "studio_demo: action handle {} was not registered",
                        action.action
                    );
                }
            }
            RuntimeEventSnapshot::SurfaceClosed(closed) if closed.surface == surface => {
                surface_closed = true;
            }
            RuntimeEventSnapshot::Action(_)
            | RuntimeEventSnapshot::SurfaceClosed(_)
            | RuntimeEventSnapshot::Diagnostic(_) => {}
        }
    }
    surface_closed
}
