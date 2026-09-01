//! Offline checking for one new-application source file.

use std::{fs, io::Write, os::unix::fs::OpenOptionsExt};

use orna_compiler::{NewApplicationCheckError, check_new_application, check_standard_library_source};
#[cfg(test)]
use orna_compiler::{CheckedClientBodyKind, CheckedFunctionId};
#[cfg(test)]
use orna_core::system::{
    SYS_INSPECT_CALLS_FUNCTION_ID, SYS_INSPECT_CALLS_TYPE_ID,
    SYS_INSPECT_INVOCATION_NODES_FUNCTION_ID, SYS_INSPECT_INVOCATION_NODES_TYPE_ID,
    SYS_INSPECT_PRESENTATION_CANDIDATES_FUNCTION_ID, SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID,
    SYS_INSPECT_RESOURCES_FUNCTION_ID, SYS_INSPECT_RESOURCES_TYPE_ID,
    SYS_INSPECT_RUNTIME_BINDINGS_FUNCTION_ID, SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID,
    SYS_INSPECT_SECURITY_DECISIONS_FUNCTION_ID, SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID,
    SYS_INSPECT_SNAPSHOT_FUNCTION_ID, SYS_INSPECT_SNAPSHOT_TYPE_ID, SYS_INSPECT_STATE_CELLS_FUNCTION_ID,
    SYS_INSPECT_STATE_CELLS_TYPE_ID, SYS_INSPECT_UI_NODES_FUNCTION_ID, SYS_INSPECT_UI_NODES_TYPE_ID,
};
use orna_core::source::{SourceBundle, SourceUnit};
use orna_standard::retained_standard_library_v11_snapshot;
use orna_standard::verify_standard_library_v11_snapshot;

#[cfg(test)]
use orna_standard::{retained_standard_library_v10_snapshot, verify_standard_library_v10_snapshot};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_new_application_source_against_verified_v10_standard() {
        let snapshot = retained_standard_library_v10_snapshot()
            .and_then(verify_standard_library_v10_snapshot)
            .expect("retained V10 standard must verify");
        let standard =
            check_standard_library_source(&snapshot).expect("verified V10 source must check");
        let source = SourceBundle::new([SourceUnit::new(
            "application.orna",
            "CREATE SCHEMA app;\n\
             CREATE CLIENT FUNCTION app.ui()\n\
             RETURNS std.ui.UI IS\n\
             BEGIN\n\
                 RETURN std.ui.text(text => 'Ready');\n\
             END;",
        )])
        .expect("application source must form a bundle");

        let report = check_new_application(&source, &standard)
            .expect("application source must be checked against V10");
        assert_eq!(report.diagnostics(), &[]);
    }
    #[test]
    fn checks_new_application_source_against_verified_v11_standard() {
        let snapshot = retained_standard_library_v11_snapshot()
            .and_then(verify_standard_library_v11_snapshot)
            .expect("retained V11 standard must verify");
        let standard =
            check_standard_library_source(&snapshot).expect("verified V11 source must check");
        let source = SourceBundle::new([SourceUnit::new(
            "application.orna",
            "CREATE SCHEMA app;\n\
             CREATE CLIENT FUNCTION app.value() RETURNS INTEGER\n\
             RETURN std.math.increment(4);",
        )])
        .expect("application source must form a bundle");
        let report = check_new_application(&source, &standard)
            .expect("application source must be checked against V11");
        assert_eq!(report.diagnostics(), &[]);
    }

    #[test]
    fn checks_static_ui_source_fixture_against_verified_v10_standard() {
        let snapshot = retained_standard_library_v10_snapshot()
            .and_then(verify_standard_library_v10_snapshot)
            .expect("retained V10 standard must verify");
        let standard =
            check_standard_library_source(&snapshot).expect("verified V10 source must check");
        let source = SourceBundle::new([SourceUnit::new(
            "static_ui_dogfood.orna",
            include_str!("../tests/fixtures/static_ui_dogfood.orna"),
        )])
        .expect("static UI application source must form a bundle");

        let report = check_new_application(&source, &standard)
            .expect("static UI application source must be checked against V10");
        assert_eq!(report.diagnostics(), &[]);
    }

    #[test]
    fn checks_client_inspector_source_fixture_against_verified_v10_standard() {
        let snapshot = retained_standard_library_v10_snapshot()
            .and_then(verify_standard_library_v10_snapshot)
            .expect("retained V10 standard must verify");
        let standard =
            check_standard_library_source(&snapshot).expect("verified V10 source must check");
        let source = SourceBundle::new([SourceUnit::new(
            "client_inspector_dogfood.orna",
            include_str!("../tests/fixtures/client_inspector_dogfood.orna"),
        )])
        .expect("client Inspector application source must form a bundle");

        let report = check_new_application(&source, &standard)
            .expect("client Inspector application source must be checked against V10");
        assert_eq!(report.diagnostics(), &[]);
        let checked = report
            .checked_bundle()
            .expect("client Inspector source produced no checked bundle");
        assert_eq!(checked.client_functions().len(), 2);

        let renderer = checked
            .client_functions()
            .find(|function| function.name().to_string() == "inspector_app.inspector_renderer")
            .expect("client Inspector renderer function is missing");
        assert_eq!(renderer.body_kind(), CheckedClientBodyKind::ExternalContract);
        let expected_carriers = [
            ("p_snapshot", SYS_INSPECT_SNAPSHOT_TYPE_ID),
            ("p_invocation_nodes", SYS_INSPECT_INVOCATION_NODES_TYPE_ID),
            ("p_calls", SYS_INSPECT_CALLS_TYPE_ID),
            ("p_resources", SYS_INSPECT_RESOURCES_TYPE_ID),
            ("p_state_cells", SYS_INSPECT_STATE_CELLS_TYPE_ID),
            ("p_ui_nodes", SYS_INSPECT_UI_NODES_TYPE_ID),
            (
                "p_presentation_candidates",
                SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID,
            ),
            ("p_runtime_bindings", SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID),
            (
                "p_security_decisions",
                SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID,
            ),
        ];
        assert_eq!(renderer.parameters().len(), expected_carriers.len());
        for (parameter, (name, type_id)) in renderer.parameters().zip(expected_carriers) {
            assert_eq!(parameter.name(), name);
            assert_eq!(
                parameter
                    .resolved_type()
                    .value()
                    .map(|value| value.type_id()),
                Some(type_id)
            );
        }

        let inspector = checked
            .client_functions()
            .find(|function| function.name().to_string() == "inspector_app.inspector")
            .expect("client Inspector function is missing");
        assert_eq!(inspector.body_kind(), CheckedClientBodyKind::Procedural);
        assert_eq!(inspector.parameters().len(), 1);
        let target = inspector
            .parameters()
            .next()
            .expect("client Inspector target parameter is missing");
        assert_eq!(
            target
                .resolved_type()
                .object_reference()
                .and_then(|reference| reference.target().existing()),
            Some(orna_core::system::SYS_INSPECT_INVOCATION_TYPE_ID)
        );

        let expected_calls = [
            CheckedFunctionId::Existing(SYS_INSPECT_SNAPSHOT_FUNCTION_ID),
            CheckedFunctionId::Existing(SYS_INSPECT_INVOCATION_NODES_FUNCTION_ID),
            CheckedFunctionId::Existing(SYS_INSPECT_CALLS_FUNCTION_ID),
            CheckedFunctionId::Existing(SYS_INSPECT_RESOURCES_FUNCTION_ID),
            CheckedFunctionId::Existing(SYS_INSPECT_STATE_CELLS_FUNCTION_ID),
            CheckedFunctionId::Existing(SYS_INSPECT_UI_NODES_FUNCTION_ID),
            CheckedFunctionId::Existing(SYS_INSPECT_PRESENTATION_CANDIDATES_FUNCTION_ID),
            CheckedFunctionId::Existing(SYS_INSPECT_RUNTIME_BINDINGS_FUNCTION_ID),
            CheckedFunctionId::Existing(SYS_INSPECT_SECURITY_DECISIONS_FUNCTION_ID),
            renderer.id(),
        ];
        let inspector_checked = checked
            .checked_bundle()
            .client_functions()
            .iter()
            .find(|function| function.id() == inspector.id())
            .expect("checked client Inspector function is missing");
        assert_eq!(
            inspector_checked.called_functions(),
            expected_calls.as_slice()
        );
    }

    #[test]
    fn rejects_retained_table_presenter_as_a_client_resource_target() {
        let snapshot = retained_standard_library_v10_snapshot()
            .and_then(verify_standard_library_v10_snapshot)
            .expect("retained V10 standard must verify");
        let standard =
            check_standard_library_source(&snapshot).expect("verified V10 source must check");
        let source = SourceBundle::new([SourceUnit::new(
            "resource.orna",
            "CREATE SCHEMA app;\n\
             CREATE CLIENT FUNCTION app.render() RETURNS BOOLEAN IS\n\
             BEGIN\n\
                 RETURN AWAIT std.data.resource(\n\
                     target => std.terminal.present_table,\n\
                     arguments => std.call.args()\n\
                 );\n\
             END;",
        )])
        .expect("resource source must form a bundle");

        let report =
            check_new_application(&source, &standard).expect("resource source must be checked");
        assert_eq!(report.diagnostics().len(), 1);
        assert_eq!(
            report.diagnostics()[0].message(),
            "unknown SERVER resource target std.terminal.present_table"
        );
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SourceCheckResult {
    Success,
    Failure,
    Usage,
}

pub(super) fn run_with_output(
    path: &str,
    output: &mut impl Write,
    human_output: bool,
    colour: bool,
) -> SourceCheckResult {
    let bytes = match read_regular_file(path) {
        Ok(bytes) => bytes,
        Err(()) => {
            return write_failure(output, &format!("orna: could not read source file: {path}"));
        }
    };
    let source = match String::from_utf8(bytes) {
        Ok(source) => source,
        Err(_) => {
            return write_failure(
                output,
                &format!("orna: source file is not valid UTF-8: {path}"),
            );
        }
    };
    let bundle = match SourceBundle::new([SourceUnit::new(path, source)]) {
        Ok(bundle) if bundle.len() == 1 => bundle,
        _ => return SourceCheckResult::Usage,
    };
    let snapshot = match retained_standard_library_v11_snapshot()
        .and_then(verify_standard_library_v11_snapshot)
    {
        Ok(snapshot) => snapshot,
        Err(_) => return write_standard_failure(output),
    };
    let standard = match check_standard_library_source(&snapshot) {
        Ok(standard) => standard,
        Err(_) => return write_standard_failure(output),
    };
    let report = match check_new_application(&bundle, &standard) {
        Ok(report) => report,
        Err(NewApplicationCheckError::SourceUnitCount { .. }) => {
            return SourceCheckResult::Usage;
        }
        Err(
            NewApplicationCheckError::Catalogue { .. } | NewApplicationCheckError::Context { .. },
        ) => return write_standard_failure(output),
        Err(_) => return write_standard_failure(output),
    };
    if report.diagnostics().is_empty() {
        return SourceCheckResult::Success;
    }
    let outcome = if report.has_errors() {
        SourceCheckResult::Failure
    } else {
        SourceCheckResult::Success
    };
    let result = if human_output {
        output.write_all(&orna_server::render_human_source_diagnostics(
            report.parse_report(),
            report.diagnostics(),
            colour,
        ))
    } else {
        output.write_all(&orna_server::render_source_diagnostics(
            report.diagnostics(),
        ))
    };
    if result.is_err() {
        SourceCheckResult::Failure
    } else {
        outcome
    }
}
fn read_regular_file(path: &str) -> Result<Vec<u8>, ()> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| ())?;
    if !file.metadata().map_err(|_| ())?.is_file() {
        return Err(());
    }
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes).map_err(|_| ())?;
    Ok(bytes)
}

fn write_standard_failure(output: &mut impl Write) -> SourceCheckResult {
    write_failure(
        output,
        "orna: embedded standard library could not be verified",
    )
}

fn write_failure(output: &mut impl Write, message: &str) -> SourceCheckResult {
    let _ = writeln!(output, "{message}");
    SourceCheckResult::Failure
}
