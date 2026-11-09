//! Typed lookup of the generated operation identities.
//!
//! The effect-handler seam is string-addressed: the evaluator receives the
//! operation name a call resolved to. That is the parse boundary. Everything past
//! this module matches on [`HostOperation`] or [`SysFunction`]; the registry is then
//! indexed by that identity rather than by a second name comparison.

use orna_sys::{
    HostOperation, HostOperationDescriptor, SysFunction, system_host_operation_registry,
};

/// The identity behind one registered effect operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RegisteredOperation {
    /// A host operation declared by the generated `sys` host ABI.
    Host(HostOperation),
    /// The engine-only entropy effect. It has no generated host descriptor, so it
    /// cannot be named by [`HostOperation`], but it still crosses the same seam.
    StdRandomEntropy,
}

/// The engine-only entropy operation, parsed once at the seam.
const STD_RANDOM_ENTROPY: &str = "std.random.entropy";

/// Parses the operation name the effect seam delivered.
pub(crate) fn registered_operation(name: &str) -> Option<RegisteredOperation> {
    if name == STD_RANDOM_ENTROPY {
        return Some(RegisteredOperation::StdRandomEntropy);
    }
    HostOperation::from_label(name).map(RegisteredOperation::Host)
}

/// Resolves a generated host identity to its runtime descriptor.
///
/// The descriptor must carry the same identity that selected it: the generated
/// registry is the authority for the operation set, and a name-only lookup could
/// otherwise return a descriptor whose fields disagree with the typed identity.
pub(crate) fn host_descriptor(
    operation: HostOperation,
) -> Option<&'static HostOperationDescriptor> {
    system_host_operation_registry()
        .operation(operation.label())
        .filter(|descriptor| descriptor.name == operation)
}

/// Parses a resolved callable label at the source/registry boundary.
pub(crate) fn sys_function(name: &str) -> Option<SysFunction> {
    SysFunction::from_label(name)
}
