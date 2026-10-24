use orna_semantic_v1::{ModuleInput, analyze};

fn show(name: &str, modules: &[(&str, &str)]) {
    let inputs: Vec<ModuleInput> = modules
        .iter()
        .map(|(path, source)| ModuleInput::new(path, source))
        .collect();
    let result = analyze(&inputs);
    println!("== {name}: ok={}", result.is_ok());
    for d in &result.diagnostics {
        println!("   diag code={} msg={}", d.code(), d.message());
    }
}

const MATH: &str = "pub fn double(value: Int): Int = value * 2;
";

#[test]
fn probe() {
    show(
        "Q1 imported free fn dot call",
        &[
            ("math.orna", MATH),
            ("main.orna", "use math.{double};\npub fn sum(n: Int): Int = n.double();\n"),
        ],
    );
    show(
        "Q2 imported free fn pipeline control",
        &[
            ("math.orna", MATH),
            ("main.orna", "use math.{double};\npub fn sum(n: Int): Int = n | double();\n"),
        ],
    );
    show(
        "Q3 imported free fn direct call control",
        &[
            ("math.orna", MATH),
            ("main.orna", "use math.{double};\npub fn sum(n: Int): Int = double(n);\n"),
        ],
    );
    show(
        "Q4 local free fn dot call on Int",
        &[(
            "main.orna",
            "pub fn double(value: Int): Int = value * 2;\npub fn sum(n: Int): Int = n.double();\n",
        )],
    );
    show(
        "N1 structurally identical nominal equality across types",
        &[(
            "main.orna",
            "pub type A {\n    value: Int,\n}\n\npub type B {\n    value: Int,\n}\n\npub fn same(): Bool = A { value: 1 } == B { value: 1 };\n",
        )],
    );
    show(
        "N2 nominal equality same type",
        &[(
            "main.orna",
            "pub type A {\n    value: Int,\n}\n\npub fn same(): Bool = A { value: 1 } == A { value: 1 };\n",
        )],
    );
    show(
        "N3 nominal ordering same type",
        &[(
            "main.orna",
            "pub type A {\n    value: Int,\n}\n\npub fn less(): Bool = A { value: 1 } < A { value: 2 };\n",
        )],
    );
    show(
        "N4 structurally identical nominal ordering across types",
        &[(
            "main.orna",
            "pub type A {\n    value: Int,\n}\n\npub type B {\n    value: Int,\n}\n\npub fn less(): Bool = A { value: 1 } < B { value: 2 };\n",
        )],
    );
    show(
        "N5 nominal value assigned to structural record type",
        &[(
            "main.orna",
            "pub type A {\n    value: Int,\n}\n\npub fn same(): Bool = A { value: 1 } == { value: 1 };\n",
        )],
    );
}
