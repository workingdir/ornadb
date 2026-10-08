// Behavior reference for the bounded regex-lite class fixture. The fixture
// declares the pattern `[a-c]{2,3}x` and the text `zbcax`, whose leftmost-longest
// match is the scalar span 1..5 (`bcax`).
const BOUNDED_CLASS_FIXTURE: &str = include_str!("fixtures/stdlib-regex-lite-bounded-class.orna");

#[test]
fn bounded_class_fixture_declares_its_pattern_and_text() {
    assert!(BOUNDED_CLASS_FIXTURE.contains("pub fn pattern(): Str = \"[a-c]{2,3}x\";"));
    assert!(BOUNDED_CLASS_FIXTURE.contains("pub fn text(): Str = \"zbcax\";"));
}
