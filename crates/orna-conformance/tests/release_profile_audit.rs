use orna_conformance::{Corpus, release_profile};

fn corpus() -> Corpus {
    Corpus::load_default().expect("checked-in conformance corpus loads")
}

#[test]
fn conformance_profile_and_coordinates_bind_to_the_1_1_0_release() {
    let corpus = corpus();
    let (profiles, coordinates) =
        release_profile::load_checked(&corpus.root).expect("profile and coordinates verify");
    assert_eq!(profiles.version, "1.1.0");
    assert_eq!(profiles.publication_revision, "final-2026-10-05");
    assert_eq!(coordinates.language, "1.1.0");
    assert_eq!(coordinates.repository_format, 3);
    assert_eq!(
        profiles.profiles["repository-engine-1.1"].required.len(),
        11
    );
}

#[test]
fn every_profile_required_orna_identifier_is_a_registered_requirement() {
    let corpus = corpus();
    let (profiles, _) =
        release_profile::load_checked(&corpus.root).expect("profile and coordinates verify");
    for (name, profile) in &profiles.profiles {
        for identifier in profile.required.iter().filter(|id| id.starts_with("ORNA-")) {
            assert!(
                corpus
                    .requirements
                    .iter()
                    .any(|requirement| &requirement.id == identifier),
                "profile {name} requires {identifier}, which the requirement register does not define"
            );
        }
        for prefix in &profile.required_prefixes {
            assert!(
                corpus
                    .requirements
                    .iter()
                    .any(|requirement| requirement.id.starts_with(prefix)),
                "profile {name} requires prefix {prefix}, which matches no registered requirement"
            );
        }
    }
}
