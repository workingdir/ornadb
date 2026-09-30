//! Attribute markers consumed by the `orna-sys-v1` build-time API collector.

use proc_macro::TokenStream;

/// Marks a real Rust method as the declaration binding for a portable `sys`
/// function. The build script reads the source attribute with `syn`; the
/// compiler-facing macro intentionally leaves the annotated method unchanged.
#[proc_macro_attribute]
pub fn ornasys(_metadata: TokenStream, item: TokenStream) -> TokenStream {
    item
}
