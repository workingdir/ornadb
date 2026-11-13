//! The production host's `HostManifest` must admit exactly the `sys` members the catalogue admission reports as
//! linked. Both sides are driven by the same production provider linkage.

use orna_check::hir_check::CatalogueAdmission;
use orna_exec::HostManifest;
use orna_hir::{SysAdmission, SysMemberStatus};
use orna_sys::{SystemSurfaceMember, system_provider_linkage};

/// The dotted `sys` path of each linked member, in the form both admissions key on.
fn linked_paths() -> Vec<String> {
    system_provider_linkage()
        .links()
        .filter_map(|link| match link.member {
            // The catalogue keys callables by bare function name; the overload suffix is not part of the path.
            SystemSurfaceMember::Callable(member) => Some(
                member
                    .label()
                    .split('(')
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
            ),
            SystemSurfaceMember::Relation(member) => Some(member.name().to_owned()),
            SystemSurfaceMember::GroupedHandle(member) => Some(member.name().to_owned()),
            SystemSurfaceMember::Singleton(member) => Some(member.name().to_owned()),
            SystemSurfaceMember::HistoricalView(_) => None,
        })
        .collect()
}

#[test]
fn host_manifest_of_the_linked_members_agrees_with_catalogue_admission() {
    let paths = linked_paths();
    assert!(
        !paths.is_empty(),
        "the production linkage must link some members"
    );

    let mut manifest = HostManifest::new();
    for path in &paths {
        manifest.register(path);
    }

    for path in &paths {
        let parts: Vec<&str> = path.split('.').collect();
        assert_eq!(
            manifest.status(&parts),
            CatalogueAdmission.status(&parts),
            "manifest and catalogue disagree on linked member {path}"
        );
        assert_eq!(manifest.status(&parts), SysMemberStatus::Linked);
    }
}

#[test]
fn host_manifest_and_catalogue_both_refuse_a_non_member() {
    // The two admissions use different vocabularies: the catalogue answers `Unknown` for a path outside the
    // catalogue, the manifest answers `Unlinked`. Parity here means neither admits the non-member.
    let manifest = HostManifest::new();
    let parts = ["sys", "definitely", "not", "a", "member"];
    assert_ne!(manifest.status(&parts), SysMemberStatus::Linked);
    assert_ne!(CatalogueAdmission.status(&parts), SysMemberStatus::Linked);
}
