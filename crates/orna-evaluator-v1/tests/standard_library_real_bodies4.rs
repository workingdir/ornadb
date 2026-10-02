use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn value(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).expect("expected value is canonical")
}

fn text(value: &str) -> CanonicalValue {
    CanonicalValue::new(Raw::Text(value.into())).expect("text is canonical")
}

fn texts(values: &[&str]) -> CanonicalValue {
    value(Raw::Array(
        values
            .iter()
            .map(|value| Raw::Text((*value).into()))
            .collect(),
    ))
}

#[test]
fn pinned_text_and_bit_exports_compute_the_documented_values() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .expect("reference standard profile is admitted");
    for import in [
        include_str!("fixtures/stdlib-use-text-z09xc.orna"),
        include_str!("fixtures/stdlib-use-bits-z09xc.orna"),
    ] {
        session
            .submit(import)
            .unwrap_or_else(|error| panic!("standard import failed with {}", error.code()));
    }

    let cases = [
        ("text.trim(\"\u{00A0}\u{3000}Orna\u{2003}\")", text("Orna")),
        (
            "text.trim(\"\u{001C}Orna\u{001F}\")",
            text("\u{001C}Orna\u{001F}"),
        ),
        (
            "text.split(\"alpha,,β,\", \",\")",
            texts(&["alpha", "", "β", ""]),
        ),
        ("text.split(\"aβ\", \"\")", texts(&["a", "β"])),
        ("text.join([\"a\", \"β\", \"\"], \":\")", text("a:β:")),
        ("text.starts_with(\"βeta\", \"β\")", value(Raw::Bool(true))),
        ("text.ends_with(\"βeta\", \"ta\")", value(Raw::Bool(true))),
        ("text.contains(\"aβb\", \"β\")", value(Raw::Bool(true))),
        ("text.replace(\"banana\", \"ana\", \"X\")", text("bXna")),
        ("text.normalise(\"é\", \"NFC\")", text("é")),
        ("text.lower(\"CAFÉ\")", text("café")),
        ("text.upper(\"straße\")", text("STRASSE")),
        ("bits.bit_or(5, 2)", value(Raw::Int(7.into()))),
        ("bits.bit_and(5, 3)", value(Raw::Int(1.into()))),
        ("bits.bit_xor(5, 3)", value(Raw::Int(6.into()))),
        ("bits.bit_not(5)", value(Raw::Int((-6).into()))),
        ("bits.shift_left(-3, 2)", value(Raw::Int((-12).into()))),
        ("bits.shift_right(-9, 2)", value(Raw::Int((-3).into()))),
        (
            "bits.bit_or(18446744073709551616, 3)",
            value(Raw::Int(18446744073709551619u128.into())),
        ),
    ];

    for (source, expected) in cases {
        let actual = session
            .submit(source)
            .unwrap_or_else(|error| panic!("{source} failed with {}", error.code()));
        assert_eq!(actual, Some(expected), "input: {source}");
    }
}
