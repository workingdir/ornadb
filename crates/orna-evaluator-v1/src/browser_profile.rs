//! Source-hosted browser profile over the shared bounded Orna evaluator.
//!
//! The exported ABI accepts source, an entry name, and caller-owned byte buffers.
//! It never imports browser APIs or exposes application, database, closure, or
//! server handles to JavaScript.

use std::fmt;
#[cfg(all(target_arch = "wasm32", feature = "browser-wasm"))]
use std::ptr;

use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, ModuleInput, SystemProviders, analyze_with_providers};
use orna_syntax_v1::{Declaration, parse_module_with_file};

use crate::{Environment, EvaluationError, Functions, Limits, PureFunction, invoke_named};

const PROFILE_SOURCE_PATH: &str = "browser/selected-app.orna";
const PROFILE_ERROR_CODE: &str = "ORNA-EVAL-BROWSER";

/// A redacted source-admission or evaluator failure in the browser profile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserProfileError {
    code: String,
}

impl BrowserProfileError {
    fn code(code: impl Into<String>) -> Self {
        Self { code: code.into() }
    }

    fn evaluation(error: EvaluationError) -> Self {
        Self::code(error.code())
    }
}

impl fmt::Display for BrowserProfileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "browser profile evaluation failed: {}", self.code)
    }
}

impl std::error::Error for BrowserProfileError {}

/// Admit selected Orna source and evaluate one named pure entry function.
///
/// Parsing, semantic admission, limits and evaluation use the same production
/// syntax, semantic catalogue and evaluator as source-hosted application code.
/// This profile supplies no effects, so database/server authority cannot enter
/// through closures or hidden host bindings.
pub fn execute_browser_profile(
    source: &str,
    entry: &str,
) -> Result<CanonicalValue, BrowserProfileError> {
    let limits = Limits::default();
    limits
        .check_source(source)
        .map_err(BrowserProfileError::evaluation)?;

    let parsed = parse_module_with_file(source, PROFILE_SOURCE_PATH);
    if !parsed.is_ok() {
        let code = parsed
            .diagnostics
            .first()
            .map_or(PROFILE_ERROR_CODE.to_owned(), |diagnostic| diagnostic.code.to_owned());
        return Err(BrowserProfileError::code(code));
    }

    let analysis = analyze_with_providers(
        &[ModuleInput::new(PROFILE_SOURCE_PATH, source.to_owned())],
        &Catalogue::authoritative_core(),
        SystemProviders::ProductionWithoutActivationSnapshot,
    );
    if !analysis.is_ok() {
        let code = analysis
            .diagnostics
            .first()
            .map_or(PROFILE_ERROR_CODE.to_owned(), |diagnostic| diagnostic.code().to_owned());
        return Err(BrowserProfileError::code(code));
    }

    let mut functions = Functions::new();
    for item in &parsed.value.items {
        if let Declaration::Function { signature, body } = &item.declaration {
            functions.insert(
                signature.name.clone(),
                PureFunction {
                    parameters: signature.parameters.clone(),
                    body: body.clone(),
                    environment: Environment::new(),
                },
            );
        }
    }
    if !functions.contains_key(entry) {
        return Err(BrowserProfileError::code("ORNA-EVAL-BROWSER-ENTRY"));
    }

    invoke_named(entry, &functions, &Environment::new(), limits)
        .map_err(BrowserProfileError::evaluation)
}

/// Allocate len bytes in the Wasm module linear memory for host input/output.
///
/// A zero length returns a non-null dangling pointer. Allocation failure returns
/// null instead of aborting the Wasm instance.
#[cfg(all(target_arch = "wasm32", feature = "browser-wasm"))]
#[unsafe(no_mangle)]
pub extern "C" fn orna_browser_profile_alloc(len: usize) -> *mut u8 {
    if len == 0 {
        return ptr::NonNull::<u8>::dangling().as_ptr();
    }
    let Ok(layout) = std::alloc::Layout::array::<u8>(len) else {
        return ptr::null_mut();
    };
    // SAFETY: Layout is valid and non-zero. A null result is propagated to the host.
    unsafe { std::alloc::alloc(layout) }
}

/// Release a buffer previously returned by the browser profile allocator.
///
/// # Safety
/// The pointer and length must be the exact pair returned by that allocator.
#[cfg(all(target_arch = "wasm32", feature = "browser-wasm"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn orna_browser_profile_dealloc(pointer: *mut u8, len: usize) {
    if pointer.is_null() || len == 0 {
        return;
    }
    let Ok(layout) = std::alloc::Layout::array::<u8>(len) else {
        return;
    };
    // SAFETY: the caller guarantees the exact pointer and length from alloc.
    unsafe { std::alloc::dealloc(pointer, layout) };
}

/// Run one selected Orna source entry through the bounded evaluator, returning
/// only canonical OVB bytes in the output buffer.
///
/// Status codes: 0 success, 1 invalid pointer, 2 invalid UTF-8, 3 failed
/// admission/evaluation/encoding, and 4 insufficient output capacity. On status
/// 4, output_len contains the required capacity and no bytes are copied.
///
/// # Safety
/// Input pointers must span readable byte ranges of the stated lengths when the
/// lengths are nonzero. output_len must be writable and aligned for usize. A
/// nonzero output capacity requires a writable output range of that length. The
/// output and output_len ranges must not overlap each other.
#[cfg(all(target_arch = "wasm32", feature = "browser-wasm"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn orna_browser_profile_execute(
    source_pointer: *const u8,
    source_len: usize,
    entry_pointer: *const u8,
    entry_len: usize,
    output_pointer: *mut u8,
    output_capacity: usize,
    output_len: *mut usize,
) -> u32 {
    if output_len.is_null()
        || (source_len != 0 && source_pointer.is_null())
        || (entry_len != 0 && entry_pointer.is_null())
        || (output_capacity != 0 && output_pointer.is_null())
    {
        return 1;
    }
    let source_pointer = if source_len == 0 {
        ptr::NonNull::<u8>::dangling().as_ptr()
    } else {
        source_pointer
    };
    let entry_pointer = if entry_len == 0 {
        ptr::NonNull::<u8>::dangling().as_ptr()
    } else {
        entry_pointer
    };
    // SAFETY: null is replaced by a non-null dangling pointer for empty input;
    // nonempty input pointers and lengths are required by the caller's contract.
    let source_bytes = unsafe { std::slice::from_raw_parts(source_pointer, source_len) };
    // SAFETY: null is replaced by a non-null dangling pointer for empty input;
    // nonempty input pointers and lengths are required by the caller's contract.
    let entry_bytes = unsafe { std::slice::from_raw_parts(entry_pointer, entry_len) };
    let Ok(source) = std::str::from_utf8(source_bytes) else {
        return 2;
    };
    let Ok(entry) = std::str::from_utf8(entry_bytes) else {
        return 2;
    };
    let Ok(value) = execute_browser_profile(source, entry) else {
        return 3;
    };
    let Ok(encoded) = value.encode() else {
        return 3;
    };
    // SAFETY: output_len was checked non-null and writable by the caller.
    unsafe { output_len.write(encoded.len()) };
    if output_capacity < encoded.len() {
        return 4;
    }
    // SAFETY: output capacity covers the encoded value and cannot overlap it.
    unsafe { ptr::copy_nonoverlapping(encoded.as_ptr(), output_pointer, encoded.len()) };
    0
}
