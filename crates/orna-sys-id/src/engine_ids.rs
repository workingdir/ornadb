//! Durable engine identities for the sealed `sys` vocabulary, committed as Rust data.
//!
//! These values are identities, not derived data: each `FunctionId` and type id is a
//! 128-bit value that persisted rows and wire bytes depend on, so it is written here
//! literally and must never change. Each value is the big-endian integer whose 32-digit
//! lowercase hex spelling is the catalogue text it replaces. The test in
//! `tests/engine_ids.rs` compares every row byte-for-byte against
//! `catalogue/engine-1.1.0.json` while that file exists.

/// The sealed engine functions in catalogue order: name and `FunctionId`.
pub const ENGINE_FUNCTION_IDS: [(&str, u128); 27] = [
    ("sys.catalog.health", 0x00000000000000000000000000000001),
    ("sys.invoke", 0x00000000000000000000000000000002),
    ("sys.source.current", 0x00000000000000000000000000000030),
    ("sys.inspect.snapshot", 0x00000000000000000000000000000003),
    (
        "sys.inspect.invocation_nodes",
        0x00000000000000000000000000000004,
    ),
    ("sys.inspect.calls", 0x00000000000000000000000000000005),
    ("sys.inspect.resources", 0x00000000000000000000000000000006),
    (
        "sys.inspect.state_cells",
        0x00000000000000000000000000000007,
    ),
    ("sys.inspect.ui_nodes", 0x00000000000000000000000000000008),
    (
        "sys.inspect.presentation_candidates",
        0x00000000000000000000000000000009,
    ),
    (
        "sys.inspect.runtime_bindings",
        0x0000000000000000000000000000000a,
    ),
    (
        "sys.inspect.security_decisions",
        0x0000000000000000000000000000000b,
    ),
    ("sys.inspect.trace", 0x0000000000000000000000000000000c),
    (
        "sys.security.session_principal",
        0x00000000000000000000000000000040,
    ),
    (
        "sys.security.effective_principal",
        0x00000000000000000000000000000041,
    ),
    (
        "sys.security.active_roles",
        0x00000000000000000000000000000042,
    ),
    (
        "sys.security.create_principal",
        0x00000000000000000000000000000043,
    ),
    (
        "sys.security.disable_principal",
        0x00000000000000000000000000000044,
    ),
    (
        "sys.security.create_role",
        0x00000000000000000000000000000045,
    ),
    (
        "sys.security.grant_role",
        0x00000000000000000000000000000046,
    ),
    (
        "sys.security.revoke_role",
        0x00000000000000000000000000000047,
    ),
    (
        "sys.security.grant_privilege",
        0x00000000000000000000000000000048,
    ),
    (
        "sys.security.revoke_privilege",
        0x00000000000000000000000000000049,
    ),
    (
        "sys.security.can_execute",
        0x0000000000000000000000000000004a,
    ),
    (
        "sys.security.has_privilege",
        0x0000000000000000000000000000004b,
    ),
    (
        "sys.state.load_user_state",
        0x0000000000000000000000000000004c,
    ),
    (
        "sys.state.write_user_state",
        0x0000000000000000000000000000004d,
    ),
];

/// The engine-private named types in catalogue order: name and type id.
pub const ENGINE_ONLY_TYPE_IDS: [(&str, u128); 4] = [
    ("sys.inspect.invocation", 0x000000000000000000000000000000f3),
    (
        "sys.inspect.snapshot_options",
        0x000000000000000000000000000000f5,
    ),
    (
        "sys.inspect.trace_event",
        0x000000000000000000000000000000f6,
    ),
    ("sys.security.principal", 0x000000000000000000000000000000f7),
];

/// Every distinct type id a sealed signature references, ascending.
///
/// Some entries have no name in the catalogue; they are identities of anonymous
/// request, parameter, result and stream types. Three of the four named engine types
/// above (`f3`, `f5`, `f6`) appear here; `sys.security.principal` (`f7`) does not.
pub const SIGNATURE_TYPE_IDS: [u128; 15] = [
    0x00000000000000000000000000000003,
    0x000000000000000000000000000000f1,
    0x000000000000000000000000000000f2,
    0x000000000000000000000000000000f3,
    0x000000000000000000000000000000f4,
    0x000000000000000000000000000000f5,
    0x000000000000000000000000000000f6,
    0x000000000000000000000000000000f8,
    0x000000000000000000000000000000f9,
    0x000000000000000000000000000000fa,
    0x000000000000000000000000000000fb,
    0x000000000000000000000000000000fc,
    0x000000000000000000000000000000fd,
    0x000000000000000000000000000000fe,
    0x000000000000000000000000000000ff,
];
