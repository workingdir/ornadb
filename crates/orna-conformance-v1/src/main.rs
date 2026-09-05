use orna_conformance_v1::{Corpus, Harness, ImplementationClaim, SyntaxAdapter};

fn main() {
    let corpus = Corpus::load_default().unwrap_or_else(|error| {
        eprintln!("cannot load authoritative Orna corpus: {error}");
        std::process::exit(2)
    });
    let mut adapter = SyntaxAdapter;
    let report = Harness::new(corpus)
        .with_claim(ImplementationClaim {
            implementation_id: "orna-conformance-v1".into(),
            profile: "syntax-parse".into(),
            command: "orna-conformance --profile syntax-parse".into(),
            environment: [
                ("adapter".into(), "SyntaxAdapter (parse-only)".into()),
                ("semantic-stages".into(), "not integrated".into()),
                ("runtime-stages".into(), "not integrated".into()),
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
