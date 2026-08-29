use std::{env, error::Error, io, path::PathBuf, process};

use orna_client::{
    CLIENT_MAX_QUEUED_RUNTIME_EVENTS, QtRuntimeExecutor, RuntimeEventSnapshot, RuntimeLibrary,
    RuntimeSession, RuntimeSessionError,
};
use orna_standard::UI_MAGIC;

const SURFACE_TITLE: &str = "OrnaDB Qt shutdown queue smoke";

fn main() {
    if let Err(error) = run() {
        eprintln!("runtime_qt_shutdown_queue_smoke: {error}");
        process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let library = RuntimeLibrary::load_qt(runtime_path()?)?;
    let session = RuntimeSession::new_qt(library, "en-GB", "UTC", "light")?;
    let mut host = QtRuntimeExecutor::new(session);
    let frame = empty_ui_frame()?;
    let surface_count = CLIENT_MAX_QUEUED_RUNTIME_EVENTS
        .checked_mul(2)
        .and_then(|count| count.checked_add(1))
        .ok_or_else(|| io::Error::other("surface count overflow"))?;
    let mut surfaces = Vec::with_capacity(surface_count);

    for _ in 0..surface_count {
        surfaces.push(host.show_window(SURFACE_TITLE, &frame)?);
    }

    for &surface in surfaces.iter().take(CLIENT_MAX_QUEUED_RUNTIME_EVENTS) {
        host.destroy_surface(surface)?;
    }

    let overflow_surface = *surfaces
        .get(CLIENT_MAX_QUEUED_RUNTIME_EVENTS)
        .ok_or_else(|| io::Error::other("no overflow surface was created"))?;
    let overflow_error = host
        .destroy_surface(overflow_surface)
        .expect_err("the bounded callback queue must reject one close event");
    if !matches!(overflow_error, RuntimeSessionError::Internal) {
        return Err(io::Error::other(format!(
            "callback queue overflow returned {overflow_error:?} instead of internal"
        ))
        .into());
    }

    let first_shutdown_error = host
        .shutdown()
        .expect_err("native shutdown must report callback queue pressure");
    if !matches!(first_shutdown_error, RuntimeSessionError::Internal) {
        return Err(io::Error::other(format!(
            "first shutdown returned {first_shutdown_error:?} instead of internal"
        ))
        .into());
    }
    host.shutdown()?;
    let events = host.drain_runtime_events();
    let closed_surfaces = events
        .iter()
        .filter(|event| matches!(event, RuntimeEventSnapshot::SurfaceClosed(_)))
        .count();
    if closed_surfaces != surface_count {
        return Err(io::Error::other(format!(
            "expected {surface_count} retained close events, received {closed_surfaces}"
        ))
        .into());
    }

    println!(
        "runtime_qt_shutdown_queue_smoke: queued {} events, rejected overflow, failed native shutdown, retried shutdown, retained {} close events",
        CLIENT_MAX_QUEUED_RUNTIME_EVENTS, closed_surfaces
    );
    Ok(())
}

fn runtime_path() -> Result<PathBuf, io::Error> {
    let mut args = env::args_os();
    let _program = args.next();
    let path = args.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: runtime_qt_shutdown_queue_smoke <runtime-shared-library>",
        )
    })?;
    if args.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: runtime_qt_shutdown_queue_smoke <runtime-shared-library>",
        ));
    }
    Ok(path.into())
}

fn empty_ui_frame() -> Result<Vec<u8>, io::Error> {
    let body = br#"{"kind":"empty"}"#;
    let body_length = u32::try_from(body.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "UI body is too large"))?;
    let mut frame = UI_MAGIC.as_bytes().to_vec();
    frame.extend_from_slice(&body_length.to_be_bytes());
    frame.extend_from_slice(body);
    Ok(frame)
}
