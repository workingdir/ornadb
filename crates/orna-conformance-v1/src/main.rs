use orna_conformance_v1::{BoundedEvaluator, Corpus, Harness, ImplementationClaim, RuntimeAdapter};

fn main() {
    let corpus = Corpus::load_default().unwrap_or_else(|error| {
        eprintln!("cannot load authoritative Orna corpus: {error}");
        std::process::exit(2)
    });
    let mut adapter = RuntimeAdapter::new(BoundedEvaluator::default());
    let report = Harness::new(corpus)
        .with_claim(ImplementationClaim {
            implementation_id: "orna-conformance-v1".into(),
            profile: "bounded-expression-runtime".into(),
            command: "orna-conformance --profile bounded-expression-runtime".into(),
            environment: [
                (
                    "adapter".into(),
                    "RuntimeAdapter (syntax, semantic analysis, and bounded expression evaluator)"
                        .into(),
                ),
                (
                    "semantic-stages".into(),
                    "semantic stages execute through the read-only v1 analyzer".into(),
                ),
                (
                    "runtime-stages".into(),
                    "pure row/expression units execute; module, effectful, and scenario stages remain explicit skips".into(),
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
