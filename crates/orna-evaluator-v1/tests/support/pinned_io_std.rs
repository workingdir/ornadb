use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};

pub fn sources() -> Vec<(String, String)> {
    orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| {
            path == "std/collection.orna"
                || path == "std/text.orna"
                || path == "std/io/main.orna"
                || path.starts_with("std/io/")
                || path == "std/encoding/main.orna"
                || path.starts_with("std/encoding/")
                || path == "std/url.orna"
                || path == "std/net/main.orna"
                || path.starts_with("std/net/")
        })
        .collect()
}

pub fn captured_profile() -> (StandardDependencyProfile, Vec<(String, String)>) {
    let sources = sources();
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/v1-reference-library#host-io-fs",
        sources.clone(),
    )
    .expect("selected IO, filesystem, encoding, and network sources form a profile");
    (profile, sources)
}

pub fn session() -> AdmittedReplSession {
    let (profile, sources) = captured_profile();
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("selected host-effect modules resolve against the captured profile");
    AdmittedReplSession::from_catalogue(&[], catalogue, sources, Limits::default())
        .expect("selected host-effect modules load without unrelated std declarations")
}
