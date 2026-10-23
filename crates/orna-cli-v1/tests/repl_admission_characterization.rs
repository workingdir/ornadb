//! CHARACTERIZATION of REPL admission through `orna-cli-v1 --db <dir> repl`.
//!
//! Every test here pins what the binary does today on `origin/main`; none of
//! them states the intended behaviour. Later slices of the REPL/CWD design
//! (R1 names the blocking item, R2 fails closed on refined aliases, R3 stops
//! routing `fn main` through serve entry) flip the specific assertions noted in
//! each test. Production code is not changed by this file.

#[path = "support/repl_run.rs"]
mod repl_run;

use repl_run::{ReplRun, initialized_project, run_lines, run_one_shot};

fn combined(output: &ReplRun) -> String {
    format!("{}{}", output.stdout, output.stderr)
}

/// Current behaviour: a namespaced non-std `pub protocol` is not admitted; the
/// REPL reports the fixed E2200 sentence and does not name the protocol.
/// R1 changes this to name `Sink` and its span; the E2200 code stays.
#[test]
fn characterizes_current_behaviour_namespaced_protocol_is_rejected_with_fixed_e2200() {
    let project = initialized_project(&[
        (
            "main.orna",
            "use sensors.greenhouse;\npub fn answer() = 1;\n",
        ),
        ("sensors/greenhouse/main.orna", "pub protocol Sink {}\n"),
    ]);

    let output = run_one_shot(project.path(), "main.answer()");
    let text = combined(&output);
    assert!(!output.status.success(), "{text}");
    assert!(text.contains("E2200"), "{text}");
    assert!(
        text.contains("project REPL session is not available"),
        "{text}"
    );
    assert!(!text.contains("Sink"), "R1 will name the item: {text}");
}

/// Current behaviour: a root refined alias is ADMITTED (the alias arm ignores
/// `refinements`), so an ordinary root function runs in a session that declares
/// it. The refinements are not evaluated by this path; R2 changes this to fail
/// closed with a named diagnostic.
#[test]
fn characterizes_current_behaviour_root_refined_alias_is_admitted_with_refinements_ignored() {
    let project = initialized_project(&[(
        "main.orna",
        "pub type Score = Int {\n    assert >= 0;\n    assert <= 100;\n}\n\
         pub fn answer() = 1;\n",
    )]);

    let output = run_lines(project.path(), &["main.answer()"]);
    let text = combined(&output);
    assert!(output.status.success(), "{text}");
    assert!(output.stdout.contains("1 : Int"), "{text}");
    assert!(!text.contains("ORNA-REPL-UNSUPPORTED"), "{text}");
}

/// Current behaviour: `fn main() = 1;` typed into a REPL session is accepted as
/// a declaration and produces no output or entry execution. ORNA-FN-003 says
/// naming a function inspects it; R3 must keep this as a session binding.
#[test]
fn characterizes_current_behaviour_fn_main_typed_in_repl_is_accepted_without_entry_execution() {
    let project = initialized_project(&[("main.orna", "pub fn answer() = 1;\n")]);

    let output = run_lines(project.path(), &["fn main() = 1;", "1 + 1"]);
    let text = combined(&output);
    assert!(output.status.success(), "{text}");
    assert!(output.stdout.contains("2 : Int"), "{text}");
    assert!(output.stderr.is_empty(), "{text}");
}

/// Current behaviour: a root top-level `assert` declaration is outside the
/// admitted REPL set (`_ => ORNA-REPL-UNSUPPORTED`), so the whole project
/// session is refused with the fixed E2200 sentence, even for an unrelated
/// root function. R1 names the `assert` declaration.
#[test]
fn characterizes_current_behaviour_root_top_level_assert_refuses_the_whole_session() {
    let project = initialized_project(&[(
        "main.orna",
        "pub fn answer() = 1;\nassert answer() == 1;\n",
    )]);

    let output = run_one_shot(project.path(), "main.answer()");
    let text = combined(&output);
    assert!(!output.status.success(), "{text}");
    assert!(text.contains("E2200"), "{text}");
}
