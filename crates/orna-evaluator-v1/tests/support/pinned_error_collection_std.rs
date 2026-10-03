use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};

pub fn captured_profile() -> (StandardDependencyProfile, Vec<(String, String)>) {
    let sources = sources();
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/v1-reference-library#error-collection-p0b5d",
        sources.clone(),
    )
    .expect("selected error and collection sources form a profile");
    (profile, sources)
}

fn sources() -> Vec<(String, String)> {
    orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| path == "std/error.orna" || path == "std/collection.orna")
        .collect()
}

pub fn session() -> AdmittedReplSession {
    let (profile, sources) = captured_profile();
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("selected error and collection modules resolve against the profile");
    AdmittedReplSession::from_catalogue(&[], catalogue, sources, Limits::default())
        .expect("selected modules load without unrelated standard declarations")
}
