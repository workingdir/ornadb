//! Quiet end when the reader of stdout has gone (`orna status | head -0`).
//!
//! Rust ignores SIGPIPE, so a write to a closed pipe fails with `EPIPE`:
//! `print!` panics on it, and explicit writes map it to a diagnostic. The
//! reader having gone is not a command failure, so this module ends the
//! process with status 0 and no diagnostic, without `unsafe`.

use std::any::Any;
use std::io;

use crate::Diagnostic;

/// Maps a failed stdout write to `failure`, unless stdout is closed: then the
/// process ends quietly, with no diagnostic.
pub(crate) fn stdout_write_failed(
    error: &io::Error,
    failure: impl FnOnce() -> Diagnostic,
) -> Diagnostic {
    if error.kind() == io::ErrorKind::BrokenPipe {
        end_quietly();
    }
    failure()
}

/// Installs a panic hook that ends quietly when a `print!`/`println!` panics
/// because stdout is closed. Every other panic keeps the default report.
pub(crate) fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if is_closed_stdout_panic(info.payload()) {
            end_quietly();
        }
        previous(info);
    }));
}

fn is_closed_stdout_panic(payload: &(dyn Any + Send)) -> bool {
    let message = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str));
    message.is_some_and(|message| {
        message.starts_with("failed printing to stdout: ") && message.contains("Broken pipe")
    })
}

/// Ends the process with status 0: a closed stdout is not a command failure.
fn end_quietly() -> ! {
    std::process::exit(0)
}

#[cfg(test)]
mod tests {
    use super::is_closed_stdout_panic;

    #[test]
    fn recognises_only_the_closed_stdout_panic() {
        let closed = String::from("failed printing to stdout: Broken pipe (os error 32)");
        assert!(is_closed_stdout_panic(&closed));
        assert!(is_closed_stdout_panic(
            &"failed printing to stdout: Broken pipe (os error 32)"
        ));
        assert!(!is_closed_stdout_panic(
            &"failed printing to stderr: Broken pipe"
        ));
        assert!(!is_closed_stdout_panic(
            &"failed printing to stdout: disk full"
        ));
        assert!(!is_closed_stdout_panic(&42_u32));
    }
}
