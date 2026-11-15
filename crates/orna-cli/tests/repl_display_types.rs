//! REPL display of a submission's static type (spec `17-repl.md`, "Eager
//! previews"): a value prints as `BODY : TYPE`.

#[path = "support/product.rs"]
mod product;

use product::*;

const BOOK_LIBRARY: &str = "pub table Book(id: Int) {\n    title: Str,\n}\n";

#[test]
fn scalar_result_prints_its_static_type() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = initialized_database(&directory, BOOK_LIBRARY);
    commit_source(&root, "display fixture");

    let output = orna_repl(&root, "1+1\n3.1415");
    assert_success(&output, "REPL scalar display");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("2 : Int"), "{}", transcript(&output));
    assert!(
        stdout.contains("3.1415 : Decimal"),
        "{}",
        transcript(&output)
    );
}
