use orna_conformance_v1::{Corpus, Harness, ImplementationClaim, SemanticAdapter};

fn main() {
    let corpus = Corpus::load_default().unwrap_or_else(|error| {
        eprintln!("cannot load authoritative Orna corpus: {error}");
        std::process::exit(2)
    });
    let mut adapter = SemanticAdapter::default();
    let report = Harness::new(corpus)
        .with_claim(ImplementationClaim {
            implementation_id: "orna-conformance-v1".into(),
            profile: "semantic-read-only".into(),
            command: "orna-conformance --profile semantic-read-only".into(),
            environment: [
                (
                    "adapter".into(),
                    "SemanticAdapter (syntax plus orna-semantic-v1)".into(),
                ),
                (
                    "semantic-stages".into(),
                    "executed through the read-only v1 analyzer".into(),
                ),
                (
                    "runtime-stages".into(),
                    "skipped: orna-runtime-v1 has no source evaluator or scenario invocation API"
                        .into(),
                ),
            ]
            .into_iter()
            .collect(),
        })
        .run(&mut adapter);
    println!(
        "{}",
        serde_json::to_string_pretty(&report).expect("report serializes")
    );
}
