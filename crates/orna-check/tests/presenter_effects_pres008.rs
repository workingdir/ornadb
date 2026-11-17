//! ORNA-PRES-008: Display and Present implementations MUST be read-only and
//! effect-free. Database writes, connectors, network, filesystem, process,
//! secret reveal and nondeterministic time or randomness are unavailable in a
//! presenter body. Each forbidden host effect is reported as E7201
//! (`DIAG_DISPLAY_READ_ONLY`), the code that already governs presenter writes.

use orna_check::{
    Analysis, Catalogue, DIAG_DISPLAY_READ_ONLY, ModuleInput, analyze_with_catalogue,
};

const SECRET_READ: &str = r#"std.secret.open(std.secret.ref("fixture.credential"), as: Str)"#;

fn presenter_source(protocol: &str, member: &str, body: &str) -> String {
    format!(
        "pub protocol {protocol} {{\n    fn {member}(self): Str;\n}}\n\
         pub table Note(id: Int) {{\n    value: Str,\n    impl {protocol} {{\n        \
         fn {member}(self): Str {{\n{body}\n        }}\n    }}\n}}\n"
    )
}

fn analyze(source: &str) -> Analysis {
    analyze_with_catalogue(
        &[ModuleInput::new("presenter.orna", source)],
        &Catalogue::authoritative_core(),
    )
}

fn has_read_only_rejection(analysis: &Analysis) -> bool {
    analysis
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code() == DIAG_DISPLAY_READ_ONLY)
}

fn assert_rejected(protocol: &str, member: &str, body: &str) {
    let analysis = analyze(&presenter_source(protocol, member, body));
    assert!(
        has_read_only_rejection(&analysis),
        "{protocol} body must be rejected as E7201: {:#?}",
        analysis.diagnostics
    );
}

#[test]
fn display_reading_a_secret_is_rejected() {
    assert_rejected(
        "Display",
        "display",
        &format!("            let token = {SECRET_READ};\n            self.value"),
    );
}

#[test]
fn present_reading_a_secret_is_rejected() {
    assert_rejected(
        "Present",
        "present",
        &format!("            let token = {SECRET_READ};\n            self.value"),
    );
}

#[test]
fn display_reading_the_clock_is_rejected() {
    assert_rejected(
        "Display",
        "display",
        "            let at = now();\n            self.value",
    );
}

#[test]
fn display_drawing_randomness_is_rejected() {
    assert_rejected(
        "Display",
        "display",
        "            let id = uuid7();\n            self.value",
    );
}

#[test]
fn pure_display_reading_its_own_field_is_accepted() {
    let analysis = analyze(&presenter_source(
        "Display",
        "display",
        "            self.value",
    ));
    assert!(
        !has_read_only_rejection(&analysis),
        "a pure presenter must not be rejected as E7201: {:#?}",
        analysis.diagnostics
    );
}
