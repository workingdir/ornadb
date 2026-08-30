use std::{env, error::Error, fmt::Write as _, io, path::PathBuf, process};

use orna_client::{
    AbiSurfaceHandle, QtRuntimeExecutor, RuntimeActionEvent, RuntimeEventSnapshot, RuntimeLibrary,
    RuntimeSession, RuntimeValueSnapshot,
};
use orna_standard::UI_MAGIC;
use serde_json::{Value, json};

const STUDIO_TITLE: &str = "Orna Studio - Qt shell";

struct Arguments {
    runtime_path: PathBuf,
    smoke: bool,
    action_smoke: bool,
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

    let surface_closed = if arguments.action_smoke {
        run_action_smoke(&mut host, surface, &canonical_state)?;
        false
    } else if arguments.smoke {
        host.poll_runtime(1)?;
        let events = host.drain_runtime_events();
        let (closed, action_id) = consume_runtime_events(&host, events, surface);
        if !closed && let Some(action_id) = action_id.as_deref() {
            let _ = apply_action_feedback(&mut host, surface, action_id)?;
        }
        println!("studio_demo: smoke poll complete");
        closed
    } else {
        println!("studio_demo: surface visible; waiting for close");
        let mut closed = false;
        while !closed {
            host.poll_runtime(50)?;
            let events = host.drain_runtime_events();
            let (next_closed, action_id) = consume_runtime_events(&host, events, surface);
            if !next_closed && let Some(action_id) = action_id.as_deref() {
                let _ = apply_action_feedback(&mut host, surface, action_id)?;
            }
            closed = next_closed;
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

fn run_action_smoke(
    host: &mut QtRuntimeExecutor,
    surface: AbiSurfaceHandle,
    previous_state: &[u8],
) -> Result<(), Box<dyn Error>> {
    host.poll_runtime(1)?;
    let action = host
        .action_handle_for_surface(surface, "studio.run")
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Studio action smoke could not resolve studio.run",
            )
        })?;
    let event = RuntimeEventSnapshot::Action(RuntimeActionEvent {
        surface,
        node: 0,
        action,
        payload: RuntimeValueSnapshot {
            handle: 0,
            type_name: "std.text".to_owned(),
            canonical_encoding: Vec::new(),
        },
    });
    let (closed, action_id) = consume_runtime_events(host, vec![event], surface);
    if closed {
        return Err(io::Error::other("Studio action smoke received an early close").into());
    }
    let action_id = action_id.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "Studio action smoke did not resolve a registered action",
        )
    })?;
    let feedback = apply_action_feedback(host, surface, &action_id)?;
    let updated_state = host.capture_semantic_state(surface)?;
    if updated_state == previous_state || !contains_text_hex(&updated_state, &feedback) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Studio action smoke did not capture the feedback update",
        )
        .into());
    }
    println!("studio_demo: action smoke feedback verified");
    Ok(())
}

fn parse_arguments() -> Result<Arguments, io::Error> {
    let mut runtime_path = None;
    let mut smoke = false;
    let mut action_smoke = false;
    for argument in env::args_os().skip(1) {
        if argument == "--smoke-action" {
            if smoke || action_smoke {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "duplicate smoke mode; usage: studio_demo <runtime-shared-library> [--smoke|--smoke-action]",
                ));
            }
            action_smoke = true;
            continue;
        }
        if argument == "--smoke" {
            if smoke || action_smoke {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "duplicate smoke mode; usage: studio_demo <runtime-shared-library> [--smoke|--smoke-action]",
                ));
            }
            smoke = true;
            continue;
        }
        if runtime_path.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected one runtime path and optional smoke mode; usage: studio_demo <runtime-shared-library> [--smoke|--smoke-action]",
            ));
        }
        runtime_path = Some(PathBuf::from(argument));
    }
    let runtime_path = runtime_path.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "missing runtime path; usage: studio_demo <runtime-shared-library> [--smoke|--smoke-action]",
        )
    })?;
    Ok(Arguments {
        runtime_path,
        smoke,
        action_smoke,
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

fn apply_action_feedback(
    host: &mut QtRuntimeExecutor,
    surface: AbiSurfaceHandle,
    action_id: &str,
) -> Result<String, Box<dyn Error>> {
    let feedback = format!("Action requested: {action_id}");
    let frame = studio_ui_frame(&feedback)?;
    host.update_window(surface, STUDIO_TITLE, &frame)?;
    println!("studio_demo: applied action feedback for {action_id}");
    Ok(feedback)
}

fn contains_text_hex(bytes: &[u8], text: &str) -> bool {
    let mut encoded = String::with_capacity(text.len() * 2);
    for byte in text.bytes() {
        write!(&mut encoded, "{byte:02x}").expect("writing hexadecimal text to String cannot fail");
    }
    bytes
        .windows(encoded.len())
        .any(|window| window == encoded.as_bytes())
}

fn consume_runtime_events(
    host: &QtRuntimeExecutor,
    events: Vec<RuntimeEventSnapshot>,
    surface: AbiSurfaceHandle,
) -> (bool, Option<String>) {
    let mut surface_closed = false;
    let mut action_id = None;
    for event in events {
        match event {
            RuntimeEventSnapshot::Action(action) if action.surface == surface => {
                if let Some(binding) = host.action_binding(action.action) {
                    let binding_id = binding.action_id().to_owned();
                    println!("studio_demo: action requested {binding_id}");
                    action_id = Some(binding_id);
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
    (surface_closed, action_id)
}
