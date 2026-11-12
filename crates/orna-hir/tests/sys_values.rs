//! `sys` value paths pass the host's admission (hir-host.md section 3.1, row HA1). A `sys` value the host has
//! not linked is rejected as the `sys` member it names, both as a field value and as a callee's base or
//! callee, and never reports as "a module, not a value". A linked `sys` declaration lowers without an
//! admission diagnostic.

use std::sync::Arc;

use orna_hir::{
    ModulePath, Name, PreludeExports, Program, SourceId, SourceUnit, SysAdmission, SysMemberStatus,
    diag::{SYS_VERSION_INCOMPATIBLE, UNSUPPORTED},
    lower_with_admission,
};
use orna_syntax::parse_module_with_file;

/// The `sys` members this test host has not linked. Every other member is linked.
const UNLINKED: [&str; 5] = [
    "Run",
    "Checkpoint",
    "catalog.objects",
    "rt.runs",
    "snapshot",
];

struct PartlyLinked;

impl SysAdmission for PartlyLinked {
    fn status(&self, path: &[&str]) -> SysMemberStatus {
        let member = match path.first() {
            Some(&"sys") => &path[1..],
            _ => path,
        }
        .join(".");
        let unlinked = UNLINKED
            .iter()
            .any(|prefix| member == *prefix || member.starts_with(&format!("{prefix}.")));
        if unlinked {
            SysMemberStatus::Unlinked
        } else {
            SysMemberStatus::Linked
        }
    }
}

/// Lowers `source` as one module under the test host's admission.
fn lowered(source: &str) -> Program {
    let parsed = parse_module_with_file(source, "test.orna");
    assert!(
        parsed.is_ok(),
        "test source must parse: {:?}",
        parsed.diagnostics
    );
    let tree = parsed.value;
    let units = [SourceUnit {
        source: SourceId::new(0),
        tree: &tree,
        path: ModulePath::new([Name::new("main")]),
    }];
    lower_with_admission(
        &units,
        &PreludeExports::default(),
        &[],
        Arc::new(PartlyLinked),
    )
}

/// Every diagnostic code the program reports.
fn codes(program: &Program) -> Vec<&str> {
    program
        .modules
        .iter()
        .flat_map(|module| module.diagnostics.iter())
        .map(|diagnostic| diagnostic.code)
        .collect()
}

#[test]
fn unlinked_sys_field_value_is_rejected() {
    let program = lowered("pub fn runs() = sys.rt.runs;\n");
    assert!(
        codes(&program).contains(&SYS_VERSION_INCOMPATIBLE),
        "an unlinked `sys` value must report sys.version.incompatible: {:?}",
        codes(&program)
    );
    assert!(
        !codes(&program).contains(&UNSUPPORTED),
        "an unlinked `sys` value is an admission rejection, not an unsupported construct: {:?}",
        codes(&program)
    );
}

#[test]
fn unlinked_sys_value_is_rejected_under_a_callee_base() {
    let program = lowered("pub fn a() = sys.Checkpoint.as_of(1);\n");
    assert!(
        codes(&program).contains(&SYS_VERSION_INCOMPATIBLE),
        "an unlinked `sys` callee must report sys.version.incompatible: {:?}",
        codes(&program)
    );
}

#[test]
fn unlinked_sys_value_is_rejected_in_a_pipeline() {
    let program = lowered(
        "pub fn c() = sys.catalog.objects | filter(object => object.kind == sys.ObjectKind.table);\n",
    );
    assert!(
        codes(&program).contains(&SYS_VERSION_INCOMPATIBLE),
        "an unlinked `sys.catalog.objects` must report sys.version.incompatible: {:?}",
        codes(&program)
    );
}

#[test]
fn unlinked_sys_function_is_rejected() {
    let program = lowered("pub fn e() = sys.snapshot(\"x\");\n");
    assert!(
        codes(&program).contains(&SYS_VERSION_INCOMPATIBLE),
        "an unlinked `sys` function must report sys.version.incompatible: {:?}",
        codes(&program)
    );
}

#[test]
fn linked_sys_enum_variant_lowers_without_admission_error() {
    let program = lowered("pub fn i() = sys.ObjectKind.table;\n");
    assert!(
        !codes(&program).contains(&SYS_VERSION_INCOMPATIBLE),
        "a linked `sys` variant must not report an admission error: {:?}",
        codes(&program)
    );
}

#[test]
fn unlinked_sys_run_value_is_rejected() {
    let program = lowered("pub fn run() = sys.Run;\n");
    assert!(
        codes(&program).contains(&SYS_VERSION_INCOMPATIBLE),
        "an unlinked `sys.Run` value must report sys.version.incompatible: {:?}",
        codes(&program)
    );
}

#[test]
fn sys_row_mutator_is_rejected_not_accepted() {
    // `sys` rows are read-only; a mutator on a linked `sys` relation must not lower as a pending method call.
    let program = lowered("pub fn bad() = sys.Commit.insert({ hash: \"x\" });\n");
    assert!(
        codes(&program).contains(&UNSUPPORTED),
        "a `sys` row mutator must be rejected, not accepted silently: {:?}",
        codes(&program)
    );
}
