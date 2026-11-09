//! Production provider linkage for the portable `sys` surface.
//!
//! The generated descriptors describe all of `api/sys.json`, but describing a
//! member does not make it executable. This table records the members for
//! which a production host links a provider. Semantic admission consults it,
//! so a program that requires any other member fails with
//! `sys.version.incompatible` before evaluation can perform writes
//! (ORNA-SYS-008) instead of type-checking and then failing mid-evaluation.
//!
//! Every catalogue member has one generated row. `None` is the explicit
//! "catalogued but unbound in this host" classification; a link is the only
//! way admission accepts the member.

use std::sync::LazyLock;

use orna_sys_id::{
    ProviderHostKind, SYS_FUNCTION_PROVIDER_HOSTS, SYS_GROUPED_HANDLE_PROVIDER_HOSTS,
    SYS_RELATION_PROVIDER_HOSTS, SYS_SINGLETON_PROVIDER_HOSTS, SysFunction, SysGroupedHandle,
    SysRelation, SysSingleton,
};

/// A portable `sys` member that admission accepts only with a linked provider.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SystemSurfaceMember {
    /// One callable overload, identified by its generated variant.
    Callable(SysFunction),
    /// Current rows of one canonical relation.
    Relation(SysRelation),
    /// Current rows of one grouped catalogue/history handle (ORNA-SYS-009).
    GroupedHandle(SysGroupedHandle),
    /// The snapshot-pinned `as_of` view of a relation, handle or singleton.
    HistoricalView(HistoricalViewMember),
    /// A singleton view, including every field read through it.
    Singleton(SysSingleton),
}

/// The underlying member of one historical (`as_of`) view.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum HistoricalViewMember {
    Relation(SysRelation),
    GroupedHandle(SysGroupedHandle),
    Singleton(SysSingleton),
}

/// The production boundary that executes a linked member.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SystemProviderHost {
    /// The evaluator executes the member natively in every host built with
    /// system host bindings.
    Evaluator,
    /// A host executes the member through the provider it installs for an
    /// operation of the generated host-operation registry.
    HostOperationRegistry,
    /// `orna-application` forwards the member to the trusted source-effect
    /// dispatcher its host supplies; `orna serve` supplies the live runtime
    /// administration dispatcher.
    LiveAdministration,
    /// The `orna run` and `orna serve` hosts read the rows from runtime
    /// publication metadata.
    PublicationMetadata,
}

/// One member and the production boundary that links its provider.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SystemProviderLink {
    pub member: SystemSurfaceMember,
    pub host: SystemProviderHost,
}

fn host(kind: ProviderHostKind) -> SystemProviderHost {
    match kind {
        ProviderHostKind::Evaluator => SystemProviderHost::Evaluator,
        ProviderHostKind::HostOperationRegistry => SystemProviderHost::HostOperationRegistry,
        ProviderHostKind::LiveAdministration => SystemProviderHost::LiveAdministration,
        ProviderHostKind::PublicationMetadata => SystemProviderHost::PublicationMetadata,
    }
}

/// The complete production provider linkage for the portable `sys` surface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemProviderLinkage {
    links: Vec<SystemProviderLink>,
}

impl SystemProviderLinkage {
    /// Every link, ordered by member and then by host.
    pub fn links(&self) -> impl Iterator<Item = &SystemProviderLink> {
        self.links.iter()
    }

    /// True when at least one production host links a provider for `member`.
    pub fn is_linked(&self, member: SystemSurfaceMember) -> bool {
        self.links.iter().any(|link| link.member == member)
    }
}

/// Builds the linkage from the generated member tables.
fn generated_linkage() -> SystemProviderLinkage {
    let mut links = Vec::new();
    for (index, classification) in SYS_FUNCTION_PROVIDER_HOSTS.iter().enumerate() {
        if let Some(kind) = classification {
            links.push(SystemProviderLink {
                member: SystemSurfaceMember::Callable(SysFunction::ALL[index]),
                host: host(*kind),
            });
        }
    }
    for (index, classification) in SYS_RELATION_PROVIDER_HOSTS.iter().enumerate() {
        if let Some(kind) = classification {
            links.push(SystemProviderLink {
                member: SystemSurfaceMember::Relation(SysRelation::ALL[index]),
                host: host(*kind),
            });
        }
    }
    for (index, classification) in SYS_GROUPED_HANDLE_PROVIDER_HOSTS.iter().enumerate() {
        if let Some(kind) = classification {
            links.push(SystemProviderLink {
                member: SystemSurfaceMember::GroupedHandle(SysGroupedHandle::ALL[index]),
                host: host(*kind),
            });
        }
    }
    for (index, classification) in SYS_SINGLETON_PROVIDER_HOSTS.iter().enumerate() {
        if let Some(kind) = classification {
            links.push(SystemProviderLink {
                member: SystemSurfaceMember::Singleton(SysSingleton::ALL[index]),
                host: host(*kind),
            });
        }
    }
    links.sort_unstable();
    links.dedup();
    SystemProviderLinkage { links }
}

/// The production provider linkage. Host-registry callables are part of the
/// generated callable classification, so there is no hand-declared link table.
pub fn system_provider_linkage() -> &'static SystemProviderLinkage {
    static LINKAGE: LazyLock<SystemProviderLinkage> = LazyLock::new(generated_linkage);
    &LINKAGE
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use orna_sys_id::{
        SYS_FUNCTION_COUNT, SYS_GROUPED_HANDLE_COUNT, SYS_RELATION_COUNT, SYS_SINGLETON_COUNT,
        SysFunction,
    };

    use super::*;

    #[test]
    fn every_catalogue_member_has_exactly_one_classification() {
        assert_eq!(SYS_FUNCTION_PROVIDER_HOSTS.len(), SYS_FUNCTION_COUNT);
        assert_eq!(SYS_RELATION_PROVIDER_HOSTS.len(), SYS_RELATION_COUNT);
        assert_eq!(
            SYS_GROUPED_HANDLE_PROVIDER_HOSTS.len(),
            SYS_GROUPED_HANDLE_COUNT
        );
        assert_eq!(SYS_SINGLETON_PROVIDER_HOSTS.len(), SYS_SINGLETON_COUNT);
        let linkage = system_provider_linkage();
        for (index, function) in SysFunction::ALL.into_iter().enumerate() {
            let linked = linkage.is_linked(SystemSurfaceMember::Callable(function));
            assert_eq!(
                linked,
                SYS_FUNCTION_PROVIDER_HOSTS[index].is_some(),
                "{}",
                function.label()
            );
        }
        for (index, relation) in SysRelation::ALL.into_iter().enumerate() {
            assert_eq!(
                linkage.is_linked(SystemSurfaceMember::Relation(relation)),
                SYS_RELATION_PROVIDER_HOSTS[index].is_some(),
                "{}",
                relation.name()
            );
        }
        for (index, handle) in SysGroupedHandle::ALL.into_iter().enumerate() {
            assert_eq!(
                linkage.is_linked(SystemSurfaceMember::GroupedHandle(handle)),
                SYS_GROUPED_HANDLE_PROVIDER_HOSTS[index].is_some(),
                "{}",
                handle.name()
            );
        }
        for (index, singleton) in SysSingleton::ALL.into_iter().enumerate() {
            assert_eq!(
                linkage.is_linked(SystemSurfaceMember::Singleton(singleton)),
                SYS_SINGLETON_PROVIDER_HOSTS[index].is_some(),
                "{}",
                singleton.name()
            );
        }
    }

    #[test]
    fn every_link_names_a_published_member_of_its_kind() {
        let document: serde_json::Value =
            serde_json::from_str(&crate::system_api_json()).expect("generated sys API is JSON");
        let names = |section: &str, key: &str| {
            document[section]
                .as_array()
                .expect("published API section is an array")
                .iter()
                .map(|entry| {
                    entry[key]
                        .as_str()
                        .expect("published API name is a string")
                        .to_owned()
                })
                .collect::<BTreeSet<_>>()
        };
        let callables = names("functions", "name");
        let relations = names("relations", "name");
        let handles = names("relations", "grouped_handle");
        let singletons = names("singletons", "name");
        for link in system_provider_linkage().links() {
            let published = match link.member {
                SystemSurfaceMember::Callable(function) => callables.contains(function.label()),
                SystemSurfaceMember::Relation(relation) => relations.contains(relation.name()),
                SystemSurfaceMember::GroupedHandle(handle) => handles.contains(handle.name()),
                SystemSurfaceMember::HistoricalView(view) => match view {
                    HistoricalViewMember::Relation(relation) => relations.contains(relation.name()),
                    HistoricalViewMember::GroupedHandle(handle) => handles.contains(handle.name()),
                    HistoricalViewMember::Singleton(singleton) => {
                        singletons.contains(singleton.name())
                    }
                },
                SystemSurfaceMember::Singleton(singleton) => singletons.contains(singleton.name()),
            };
            assert!(published, "{link:?} does not name a published sys member");
        }
    }

    #[test]
    fn only_the_generating_registry_callable_uses_the_host_registry() {
        let linked = system_provider_linkage()
            .links()
            .filter(|link| link.host == SystemProviderHost::HostOperationRegistry)
            .map(|link| match link.member {
                SystemSurfaceMember::Callable(function) => function,
                other => panic!("host registry links only callables, found {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(linked, [SysFunction::BlobCaptureFile]);
    }

    #[test]
    fn members_without_a_production_provider_are_not_linked() {
        let linkage = system_provider_linkage();
        for member in [
            SystemSurfaceMember::Callable(SysFunction::SnapshotStr),
            SystemSurfaceMember::Callable(SysFunction::ExplainFunctionRef),
            SystemSurfaceMember::Relation(SysRelation::Plan),
            SystemSurfaceMember::Callable(SysFunction::StartValue),
            SystemSurfaceMember::Callable(SysFunction::Cancel),
            SystemSurfaceMember::Relation(SysRelation::Run),
            SystemSurfaceMember::HistoricalView(HistoricalViewMember::Relation(
                SysRelation::Storage,
            )),
            SystemSurfaceMember::HistoricalView(HistoricalViewMember::Singleton(
                SysSingleton::Database,
            )),
            SystemSurfaceMember::Singleton(SysSingleton::Rt),
        ] {
            assert!(!linkage.is_linked(member), "{member:?}");
        }
        for member in [
            SystemSurfaceMember::Callable(SysFunction::BlobAnnotate),
            SystemSurfaceMember::Callable(SysFunction::BlobMediaType),
            SystemSurfaceMember::Callable(SysFunction::BlobSuffix),
            SystemSurfaceMember::Callable(SysFunction::BlobSameContent),
            SystemSurfaceMember::Callable(SysFunction::ExplainQuery),
            SystemSurfaceMember::Callable(SysFunction::RtInfo),
            SystemSurfaceMember::Callable(SysFunction::BlobCaptureFile),
            SystemSurfaceMember::Relation(SysRelation::Storage),
            SystemSurfaceMember::GroupedHandle(SysGroupedHandle::CatalogTables),
        ] {
            assert!(linkage.is_linked(member), "{member:?}");
        }
    }
}
