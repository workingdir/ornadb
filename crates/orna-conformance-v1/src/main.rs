use orna_conformance_v1::{Corpus, Harness, SkippingAdapter};

fn main() {
    let corpus = Corpus::load_default().unwrap_or_else(|error| {
        eprintln!("cannot load authoritative Orna corpus: {error}");
        std::process::exit(2)
    });
    let mut adapter = SkippingAdapter;
    let report = Harness::new(corpus).run(&mut adapter);
    println!(
        "{}",
        serde_json::to_string_pretty(&report).expect("report serializes")
    );
}
