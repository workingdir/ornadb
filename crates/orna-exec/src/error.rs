//! Non-value exits from evaluation.

use crate::value::Value;

/// The public result of running an entry point that did not produce a value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecError {
    /// The program ended with a language error, named by its canonical code.
    Code(&'static str),
    /// The program reached a construct this stage does not execute yet.
    NotYet(&'static str),
}

/// Internal non-local exits. `Return`, `Break` and `Continue` are control
/// flow, not errors; they are caught by the function, loop that owns them.
#[derive(Debug)]
pub(crate) enum Stop {
    /// A language error with its canonical code.
    Error(&'static str),
    /// A construct this stage does not execute yet.
    NotYet(&'static str),
    /// `return` with its value.
    Return(Value),
    /// `break`, which a loop catches.
    Break,
    /// `continue`, which a loop catches.
    Continue,
}

impl From<Stop> for ExecError {
    fn from(stop: Stop) -> Self {
        match stop {
            Stop::Error(code) => Self::Code(code),
            Stop::NotYet(reason) => Self::NotYet(reason),
            Stop::Return(_) | Stop::Break | Stop::Continue => {
                Self::NotYet("control flow outside a loop")
            }
        }
    }
}
