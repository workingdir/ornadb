//! Production provider linkage for the portable `sys` surface.
//!
//! The generated descriptors describe all of `api/sys.json`, but describing a
//! member does not make it executable. This table records the members for
//! which a production host links a provider. Semantic admission consults it,
//! so a program that requires any other member fails with
//! `sys.version.incompatible` before evaluation can perform writes
//! (ORNA-SYS-008) instead of type-checking and then failing mid-evaluation.
//!
//! Callables served through the generated host-operation registry are derived
//! from that registry. Every other provider is dispatched by a crate that
//! depends on this one, so those links are declared here and tests in the
//! dispatching crates cross-check them against the dispatch sites.

use std::sync::LazyLock;

use crate::{system_dispatch_table, system_host_operation_registry};

/// A portable `sys` member that admission accepts only with a linked provider.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SystemSurfaceMember<'a> {
    /// One callable overload, named by its exact `api/sys.json` label.
    Callable(&'a str),
    /// Current rows read through one relation path. A canonical relation and
    /// its grouped handle are separate paths: a host that recognises one does
    /// not necessarily recognise the other.
    Relation(&'a str),
    /// The snapshot-pinned `as_of` view read through one relation path or
    /// singleton.
    HistoricalView(&'a str),
    /// A singleton view, including every field read through it.
    Singleton(&'a str),
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
    /// `orna-application-v1` forwards the member to the trusted source-effect
    /// dispatcher its host supplies; `orna serve` supplies the live runtime
    /// administration dispatcher.
    LiveAdministration,
    /// The `orna run` and `orna serve` hosts read the rows from runtime
    /// publication metadata.
    PublicationMetadata,
    /// The `orna run` and `orna serve` hosts page the rows of the activation's
    /// admitted table snapshot.
    ActivationTableSnapshot,
}

/// One member and the production boundary that links its provider.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SystemProviderLink {
    pub member: SystemSurfaceMember<'static>,
    pub host: SystemProviderHost,
}

/// Links whose dispatch site is in a crate that depends on this one.
const DECLARED_LINKS: [SystemProviderLink; 11] = [
    // `orna-evaluator-v1`: `Evaluator::call` dispatches the annotation natively.
    SystemProviderLink {
        member: SystemSurfaceMember::Callable("sys.blob.annotate"),
        host: SystemProviderHost::Evaluator,
    },
    // `orna-evaluator-v1`: `Evaluator::call` answers the metadata-only
    // reflection and exact content equality of Blob values natively.
    SystemProviderLink {
        member: SystemSurfaceMember::Callable("sys.blob.media_type"),
        host: SystemProviderHost::Evaluator,
    },
    SystemProviderLink {
        member: SystemSurfaceMember::Callable("sys.blob.suffix"),
        host: SystemProviderHost::Evaluator,
    },
    SystemProviderLink {
        member: SystemSurfaceMember::Callable("sys.blob.same_content"),
        host: SystemProviderHost::Evaluator,
    },
    // `orna-evaluator-v1`: `Evaluator::call` lowers the query natively through
    // `explain_bindings`; the plan pins its source through the catalogue the
    // host installs. No production host installs an `ExplainCatalogue` yet
    // (A10-sys-inventory-03, bead ornadb-5il1.124), so `orna run`, `orna serve`
    // and REPL calls fail with `sys.version.incompatible` until one does. The
    // `FunctionRef` and `Diagnostic` overloads stay unlinked: the evaluator
    // rejects them.
    SystemProviderLink {
        member: SystemSurfaceMember::Callable("sys.explain(Query)"),
        host: SystemProviderHost::Evaluator,
    },
    // `orna-evaluator-v1`: `Evaluator::call` answers `sys.rt.info()` natively
    // from the `SysHostContext` the host attaches to the activation. A host
    // that attaches none reports `sys.version.incompatible` (SYS-022..030).
    SystemProviderLink {
        member: SystemSurfaceMember::Callable("sys.rt.info"),
        host: SystemProviderHost::Evaluator,
    },
    // `orna-application-v1`: `AsyncSourceMutationEffectHandler` forwards both
    // stream transitions to the trusted source-effect dispatcher, and the
    // `orna-live-v1` dispatcher of `orna serve` executes them. `sys.cancel`
    // takes the same route but stays unlinked: every production dispatcher
    // answers it with a constant failure and no production source produces a
    // `sys.InvocationHandle` to cancel.
    SystemProviderLink {
        member: SystemSurfaceMember::Callable("sys.admin.pause_stream"),
        host: SystemProviderHost::LiveAdministration,
    },
    SystemProviderLink {
        member: SystemSurfaceMember::Callable("sys.admin.resume_stream"),
        host: SystemProviderHost::LiveAdministration,
    },
    // `orna-evaluator-v1` recognises both canonical names as relation
    // sources; the run and serve hosts page their publication metadata rows.
    SystemProviderLink {
        member: SystemSurfaceMember::Relation("sys.MaintenanceJob"),
        host: SystemProviderHost::PublicationMetadata,
    },
    SystemProviderLink {
        member: SystemSurfaceMember::Relation("sys.Storage"),
        host: SystemProviderHost::PublicationMetadata,
    },
    // `orna-evaluator-v1` recognises the grouped catalogue handle as a relation
    // source; the run and serve hosts page it from the activation table
    // snapshot, so admission only links it on those hosts.
    SystemProviderLink {
        member: SystemSurfaceMember::Relation("sys.catalog.tables"),
        host: SystemProviderHost::ActivationTableSnapshot,
    },
];

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
    pub fn is_linked(&self, member: SystemSurfaceMember<'_>) -> bool {
        self.links.iter().any(|link| link.member == member)
    }

    /// True when a production host other than `excluded` links a provider for
    /// `member`. A host that serves no activation table snapshot excludes
    /// [`SystemProviderHost::ActivationTableSnapshot`] so it never admits a
    /// member only that host can execute (ORNA-SYS-008).
    pub fn is_linked_except(&self, member: SystemSurfaceMember<'_>, excluded: SystemProviderHost) -> bool {
        self.links
            .iter()
            .any(|link| link.member == member && link.host != excluded)
    }
}

/// The production provider linkage. Host-registry callables are derived from
/// the generated host-operation registry; the other links are declared above.
pub fn system_provider_linkage() -> &'static SystemProviderLinkage {
    static LINKAGE: LazyLock<SystemProviderLinkage> = LazyLock::new(|| {
        let dispatch = system_dispatch_table();
        let registered = system_host_operation_registry()
            .operations()
            .filter(|operation| dispatch.operation(&operation.name).is_some())
            .map(|operation| SystemProviderLink {
                member: SystemSurfaceMember::Callable(operation.name.as_str()),
                host: SystemProviderHost::HostOperationRegistry,
            });
        let mut links = DECLARED_LINKS
            .into_iter()
            .chain(registered)
            .collect::<Vec<_>>();
        links.sort_unstable();
        links.dedup();
        SystemProviderLinkage { links }
    });
    &LINKAGE
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    struct PortableNames {
        callables: BTreeSet<String>,
        relations: BTreeSet<String>,
        grouped_handles: BTreeSet<String>,
        singletons: BTreeSet<String>,
    }

    fn portable_names() -> PortableNames {
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
        PortableNames {
            callables: names("functions", "name"),
            relations: names("relations", "name"),
            grouped_handles: names("relations", "grouped_handle"),
            singletons: names("singletons", "name"),
        }
    }

    #[test]
    fn every_link_names_a_published_member_of_its_kind() {
        let names = portable_names();
        for link in system_provider_linkage().links() {
            let published = match link.member {
                SystemSurfaceMember::Callable(label) => names.callables.contains(label),
                SystemSurfaceMember::Relation(path) => {
                    names.relations.contains(path) || names.grouped_handles.contains(path)
                }
                SystemSurfaceMember::HistoricalView(path) => {
                    names.relations.contains(path)
                        || names.grouped_handles.contains(path)
                        || names.singletons.contains(path)
                }
                SystemSurfaceMember::Singleton(name) => names.singletons.contains(name),
            };
            assert!(published, "{link:?} does not name a published sys member");
        }
    }

    #[test]
    fn host_registry_callables_are_derived_from_the_generated_registry() {
        let dispatch = system_dispatch_table();
        let registered = system_host_operation_registry()
            .operations()
            .filter(|operation| dispatch.operation(&operation.name).is_some())
            .map(|operation| operation.name.as_str())
            .collect::<BTreeSet<_>>();
        let linked = system_provider_linkage()
            .links()
            .filter(|link| link.host == SystemProviderHost::HostOperationRegistry)
            .map(|link| match link.member {
                SystemSurfaceMember::Callable(label) => label,
                other => panic!("host registry links only callables, found {other:?}"),
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(linked, registered);
        assert!(linked.contains("sys.blob.capture_file"));
    }

    #[test]
    fn declared_links_are_retained_exactly_once() {
        let linkage = system_provider_linkage();
        for declared in DECLARED_LINKS {
            assert_eq!(
                linkage.links().filter(|link| **link == declared).count(),
                1,
                "{declared:?}"
            );
        }
        let links = linkage.links().collect::<Vec<_>>();
        assert!(links.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn members_without_a_production_provider_are_not_linked() {
        let linkage = system_provider_linkage();
        for member in [
            SystemSurfaceMember::Callable("sys.snapshot(Str)"),
            SystemSurfaceMember::Callable("sys.explain(FunctionRef)"),
            SystemSurfaceMember::Relation("sys.Plan"),
            SystemSurfaceMember::Callable("sys.start(Value)"),
            SystemSurfaceMember::Callable("sys.cancel"),
            SystemSurfaceMember::Relation("sys.Run"),
            SystemSurfaceMember::Relation("sys.storage.objects"),
            SystemSurfaceMember::HistoricalView("sys.Storage"),
            SystemSurfaceMember::HistoricalView("sys.database"),
            SystemSurfaceMember::Singleton("sys.rt"),
            SystemSurfaceMember::Singleton("sys.database"),
        ] {
            assert!(!linkage.is_linked(member), "{member:?}");
        }
        for member in [
            SystemSurfaceMember::Callable("sys.blob.annotate"),
            SystemSurfaceMember::Callable("sys.blob.media_type"),
            SystemSurfaceMember::Callable("sys.blob.suffix"),
            SystemSurfaceMember::Callable("sys.blob.same_content"),
            SystemSurfaceMember::Callable("sys.explain(Query)"),
            SystemSurfaceMember::Callable("sys.rt.info"),
            SystemSurfaceMember::Callable("sys.blob.capture_file"),
            SystemSurfaceMember::Relation("sys.Storage"),
        ] {
            assert!(linkage.is_linked(member), "{member:?}");
        }
    }

    #[test]
    fn snapshot_only_members_are_not_linked_when_their_host_is_excluded() {
        let linkage = system_provider_linkage();
        let snapshot_only = SystemSurfaceMember::Relation("sys.catalog.tables");
        assert!(linkage.is_linked(snapshot_only));
        assert!(linkage.is_linked_except(snapshot_only, SystemProviderHost::HostOperationRegistry));
        assert!(!linkage.is_linked_except(
            snapshot_only,
            SystemProviderHost::ActivationTableSnapshot,
        ));
        let always_linked = SystemSurfaceMember::Callable("sys.rt.info");
        assert!(linkage.is_linked_except(
            always_linked,
            SystemProviderHost::ActivationTableSnapshot,
        ));
    }
}
