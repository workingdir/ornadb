use super::*;

#[test]
fn application_checker_accepts_scalar_server_select_and_retains_return_identity() {
    let source = "CREATE SCHEMA app;\nCREATE TYPE app.item AS OBJECT (value INTEGER);\nCREATE SERVER FUNCTION app.echo()\nRETURNS INTEGER\nSECURITY INVOKER\nTRANSACTION READ ONLY\nVOLATILITY STABLE\nAS\n    SELECT i.value FROM app.item i;";
    let report = check(&bundle([("app.orna", source)]), &empty_catalogue());

    assert_eq!(report.diagnostics(), &[]);
    let function = &report.checked_bundle().unwrap().server_functions()[0];
    assert!(matches!(
        function.return_type(),
        super::super::CheckedServerFunctionReturn::Single {
            semantic_type: SemanticType::Scalar(StandardScalar::Integer),
            standard_value_type: None,
            ..
        }
    ));
}

#[test]
fn scalar_server_select_rejects_projection_count_mismatch() {
    let source = "CREATE SCHEMA app; CREATE TYPE app.item AS OBJECT (value INTEGER); CREATE SERVER FUNCTION app.echo() RETURNS INTEGER SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT i.value, i.value FROM app.item i;";
    let report = check(&bundle([("app.orna", source)]), &empty_catalogue());

    assert!(
        report
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code() == DiagnosticCode::TypeMismatch)
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn scalar_server_select_rejects_projection_type_mismatch() {
    let source = "CREATE SCHEMA app; CREATE TYPE app.item AS OBJECT (value INTEGER); CREATE SERVER FUNCTION app.echo() RETURNS TEXT SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT i.value FROM app.item i;";
    let report = check(&bundle([("app.orna", source)]), &empty_catalogue());

    assert!(
        report
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code() == DiagnosticCode::TypeMismatch)
    );
    assert_no_checked_bundle(&report);
}
