use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};

fn admitted_session(
    profile_name: &str,
    include_module: impl Fn(&str) -> bool,
) -> AdmittedReplSession {
    let sources = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| include_module(path))
        .collect::<Vec<_>>();
    let profile = StandardDependencyProfile::from_sources(profile_name, sources.clone())
        .expect("selected source bytes form a standard dependency profile");
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("selected standard modules resolve against the captured profile");
    AdmittedReplSession::from_catalogue(&[], catalogue, sources, Limits::default()).unwrap_or_else(
        |error| panic!("selected standard modules failed to load: {}", error.code()),
    )
}

#[allow(dead_code)]
pub fn time_session() -> AdmittedReplSession {
    admitted_session("orna.std/1b5ob-time-contracts", |path| {
        path == "std/time.orna"
            || path == "std/time/calendar.orna"
            || path.starts_with("std/time/duration/")
    })
}

#[allow(dead_code)]
pub fn text_math_session() -> AdmittedReplSession {
    admitted_session("orna.std/1b5ob-text-contracts", |path| {
        path == "std/text.orna" || path == "std/math.orna"
    })
}

#[allow(dead_code)]
pub fn concurrent_session() -> AdmittedReplSession {
    admitted_session("orna.std/x6aj2-concurrent-time-contracts", |path| {
        path == "std/concurrent/main.orna"
    })
}
