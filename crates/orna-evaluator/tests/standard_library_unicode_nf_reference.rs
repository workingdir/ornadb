// Reference for the decomposed NFC fixture. The fixture declares that
// normalising `e` followed by U+0301 under NFC yields the precomposed `é`
// (U+00E9).
const DECOMPOSED_NFC_FIXTURE: &str =
    include_str!("fixtures/stdlib-text-normalise-decomposed-nfc-proof.orna");

#[test]
fn decomposed_nfc_fixture_declares_composition_to_precomposed_e_acute() {
    assert!(DECOMPOSED_NFC_FIXTURE.contains("text_ops.normalise(\"Cafe\u{301}\", \"NFC\")"));
    assert!(DECOMPOSED_NFC_FIXTURE.contains("== \"Caf\u{e9}\""));
}
