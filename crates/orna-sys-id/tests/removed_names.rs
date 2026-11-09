//! The four `removed_names` decisions, including the ORNA-SYS-005 suggestions.

use orna_sys_id::{
    DiagnosticIdentity, ORNA100_E_SYS_RUNTIME, PortableRemoval, RuleCode, SYS_REMOVED_NAME_COUNT,
    SysFailureCode, SysField, SysFunction, SysMemberPath, SysRemovedName, SysSingleton,
};

fn removal(name: SysRemovedName) -> PortableRemoval {
    name.removal()
}

#[test]
fn the_catalogue_removals_are_closed_identities() {
    assert_eq!(SYS_REMOVED_NAME_COUNT, 4);
    assert_eq!(SysRemovedName::ALL.len(), 4);
    let mut names: Vec<&str> = SysRemovedName::ALL.iter().map(|name| name.name()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "sys.admin.rewrite_storage",
            "sys.admin.set_storage_preference",
            "sys.runtime",
            "sys.runtime_info",
        ]
    );
    for name in SysRemovedName::ALL {
        assert_eq!(SysRemovedName::parse(name.name()), Some(name));
        assert_eq!(name.name().parse::<SysRemovedName>().unwrap(), name);
        assert_eq!(name.to_string(), name.name());
        assert_eq!(
            removal(name).diagnostic().as_str(),
            expected_diagnostic(name)
        );
    }
}

fn expected_diagnostic(name: SysRemovedName) -> &'static str {
    match name {
        SysRemovedName::Runtime | SysRemovedName::RuntimeInfo => "ORNA100-E-SYS-RUNTIME",
        SysRemovedName::AdminSetStoragePreference | SysRemovedName::AdminRewriteStorage => {
            "sys.version.incompatible"
        }
    }
}

#[test]
fn orna_sys_005_suggestions_are_member_paths_not_strings() {
    // ORNA-SYS-005: `sys.runtime` reports ORNA100-E-SYS-RUNTIME and suggests `sys.rt`.
    let runtime = removal(SysRemovedName::Runtime);
    assert_eq!(
        runtime,
        PortableRemoval::Replaced {
            by: SysMemberPath::Singleton(SysSingleton::Rt),
            diagnostic: DiagnosticIdentity::Rule(ORNA100_E_SYS_RUNTIME),
        }
    );
    match runtime {
        PortableRemoval::Replaced { by, diagnostic } => {
            assert_eq!(by.to_string(), "sys.rt");
            assert_eq!(by, SysMemberPath::Singleton(SysSingleton::Rt));
            assert_eq!(diagnostic.as_str(), "ORNA100-E-SYS-RUNTIME");
        }
        PortableRemoval::Retired { .. } => panic!("sys.runtime is replaced, not retired"),
    }

    // The catalogue publishes `sys.rt.info` as a callable, not as a field of
    // `sys.RuntimeView`, so the closed path is the callable identity. The build
    // rejects any attempt to name a field the view does not declare.
    let runtime_info = removal(SysRemovedName::RuntimeInfo);
    assert_eq!(
        runtime_info,
        PortableRemoval::Replaced {
            by: SysMemberPath::Callable(SysFunction::RtInfo),
            diagnostic: DiagnosticIdentity::Rule(ORNA100_E_SYS_RUNTIME),
        }
    );
    match runtime_info {
        PortableRemoval::Replaced { by, diagnostic } => {
            assert_eq!(by.to_string(), "sys.rt.info");
            assert_eq!(diagnostic.as_str(), "ORNA100-E-SYS-RUNTIME");
        }
        PortableRemoval::Retired { .. } => panic!("sys.runtime_info is replaced, not retired"),
    }
    assert_eq!(
        SysMemberPath::singleton_field(SysSingleton::Rt, SysField::Writable),
        None,
        "sys.RuntimeView does not declare `writable`"
    );
    assert_eq!(
        SysMemberPath::singleton_field(SysSingleton::Database, SysField::Id),
        Some(SysMemberPath::SingletonField(
            SysSingleton::Database,
            SysField::Id
        ))
    );
}

#[test]
fn retired_names_keep_the_catalogue_reason_and_failure_code() {
    for name in [
        SysRemovedName::AdminSetStoragePreference,
        SysRemovedName::AdminRewriteStorage,
    ] {
        match removal(name) {
            PortableRemoval::Retired { diagnostic, reason } => {
                assert_eq!(
                    diagnostic,
                    DiagnosticIdentity::Failure(SysFailureCode::VersionIncompatible)
                );
                assert_eq!(diagnostic.as_str(), "sys.version.incompatible");
                assert_eq!(
                    reason,
                    "No new-format row placement mode; migrate source explicitly."
                );
            }
            PortableRemoval::Replaced { .. } => panic!("`{}` is retired", name.name()),
        }
    }
}

#[test]
fn rule_codes_are_closed_and_print_their_bytes() {
    assert_eq!(ORNA100_E_SYS_RUNTIME.as_str(), "ORNA100-E-SYS-RUNTIME");
    assert_eq!(ORNA100_E_SYS_RUNTIME.to_string(), "ORNA100-E-SYS-RUNTIME");
    assert_eq!(
        RuleCode::new("ORNA100-E-SYS-RUNTIME"),
        ORNA100_E_SYS_RUNTIME
    );
    assert_eq!(
        DiagnosticIdentity::Rule(ORNA100_E_SYS_RUNTIME).to_string(),
        "ORNA100-E-SYS-RUNTIME"
    );
}
