//! Test-only route trace for the evaluator's call cascade.
//!
//! Enabled by the `route-trace` Cargo feature, which is off in every shipped
//! build: with the feature off the recording statements at the exits of
//! `Context::call` are compiled out entirely. With it on, each exit records
//! which branch of the cascade (design section 1.3) served the call, together
//! with the callee's span.
//!
//! The differential harness (design section 6.2) and the H1 call-classification
//! assertion consume these records instead of reading the evaluator's control
//! flow. The feature and this module are deleted in stage H9.
//!
//! Records are collected per thread: [`collect`] installs a collector on the
//! calling thread and none elsewhere. Evaluations that hand work to another
//! thread record nothing there, which is why a collector must wrap the
//! evaluation itself rather than run beside it.

use std::cell::RefCell;

use orna_syntax::{Expr, SyntaxSpan};

/// Which branch of the evaluator's call cascade served one call, in the order
/// the cascade tries them.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CallKind {
    /// A decimal's `.divide` method.
    DecimalDivide,
    /// A codec `decode` (`std.encoding.json.decode` and its siblings).
    CodecDecode,
    /// `std.ui.action`.
    UiAction,
    /// A root intrinsic (`Some`, `now`, `uuid7`, `error`, `fail`).
    RootIntrinsic,
    /// A relation source.
    RelationSource,
    /// A relation pipeline stage.
    RelationStage,
    /// The system host (`sys.*` and registered host operations).
    SystemHost,
    /// A native operation chosen by `route_call`.
    Native,
    /// A source function or closure.
    Source,
}

impl CallKind {
    /// Every kind, in cascade order.
    pub const ALL: [CallKind; 9] = [
        CallKind::DecimalDivide,
        CallKind::CodecDecode,
        CallKind::UiAction,
        CallKind::RootIntrinsic,
        CallKind::RelationSource,
        CallKind::RelationStage,
        CallKind::SystemHost,
        CallKind::Native,
        CallKind::Source,
    ];
}

/// One executed call as the cascade saw it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallRecord {
    /// The callee expression's span.
    pub span: SyntaxSpan,
    /// Which cascade branch served the call.
    pub kind: CallKind,
}

thread_local! {
    static COLLECTOR: RefCell<Option<Vec<CallRecord>>> = const { RefCell::new(None) };
}

/// Records one call when a collector is installed on this thread.
pub(crate) fn record(callee: &Expr, kind: CallKind) {
    let span = callee.span().clone();
    COLLECTOR.with(|collector| {
        if let Some(records) = collector.borrow_mut().as_mut() {
            records.push(CallRecord { span, kind });
        }
    });
}

struct Installed;

impl Drop for Installed {
    fn drop(&mut self) {
        COLLECTOR.with(|collector| *collector.borrow_mut() = None);
    }
}

/// Runs `run` with a call collector installed and returns its result with the
/// calls it executed, in execution order.
///
/// # Panics
///
/// Panics when a collector is already installed on this thread; nested
/// collection has no defined record ordering.
pub fn collect<T>(run: impl FnOnce() -> T) -> (T, Vec<CallRecord>) {
    let previous = COLLECTOR.with(|collector| collector.borrow_mut().replace(Vec::new()));
    assert!(
        previous.is_none(),
        "route-trace collectors cannot nest on one thread"
    );
    let installed = Installed;
    let value = run();
    let records = COLLECTOR
        .with(|collector| collector.borrow_mut().take())
        .unwrap_or_default();
    drop(installed);
    (value, records)
}
