use orna_evaluator_v1::{
    CancellationToken, ContextValueEffectHandler, EffectHandler, Environment, EvaluationError,
    Functions, Limits, Ovb2EffectHandler, PureFunction, StepBudget, SysHostBindingRegistry,
    invoke_named_with_context_effects,
};
use orna_foundation_v1::CanonicalValue;
use orna_syntax_v1::{Expr, parse_module};
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};
use orna_value_v1::{ContextValue, Raw, ValueFormat};

#[path = "support/format3.rs"]
mod format3;

struct FixtureEffects {
    host: SysHostBindingRegistry,
    persisted: Option<ContextValue>,
    encoded_row: Option<Vec<u8>>,
}

impl EffectHandler for FixtureEffects {
    fn handle(
        &mut self,
        _callee: &Expr,
        _arguments: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        Err(EvaluationError::redacted(
            orna_foundation_v1::SafeText::new("ORNA-EVAL-UNSUPPORTED".to_owned())
                .expect("static evaluator error code"),
        ))
    }
}

impl ContextValueEffectHandler for FixtureEffects {
    fn handle_context(
        &mut self,
        _callee: &Expr,
        arguments: &[ContextValue],
    ) -> Result<Option<ContextValue>, EvaluationError> {
        let [tuple] = arguments else {
            return Err(EvaluationError::redacted(
                orna_foundation_v1::SafeText::new("ORNA-EVAL-ARGUMENT".to_owned())
                    .expect("static evaluator error code"),
            ));
        };
        if tuple.format() != ValueFormat::Ovb2 {
            return Err(EvaluationError::redacted(
                orna_foundation_v1::SafeText::new("ORNA-EVAL-VALUE".to_owned())
                    .expect("static evaluator error code"),
            ));
        }
        let Raw::Tag(60015, boxed) = tuple.raw() else {
            panic!("persist receives a source tuple")
        };
        let Raw::Array(fields) = boxed.as_ref() else {
            panic!("source tuple has array fields")
        };
        let blob = ContextValue::new(ValueFormat::Ovb2, fields[0].clone())
            .expect("tuple carries a valid annotated Blob");
        let annotated = blob.blob().expect("captured Blob remains annotated");
        assert_eq!(annotated.media_type(), "audio/mp4");
        assert_eq!(annotated.suffix(), Some("m4b"));
        let binding = self.host.accept_captured_blob_for_row(&blob)?;
        self.encoded_row = Some(binding.encoded_value().to_vec());
        self.persisted = Some(tuple.clone());
        Ok(Some(tuple.clone()))
    }

    fn handle_context_registered_with_budget(
        &mut self,
        operation: &str,
        callee: &Expr,
        arguments: &[ContextValue],
        budget: &mut StepBudget,
    ) -> Result<Option<ContextValue>, EvaluationError> {
        if operation == "sys.blob.capture_file" {
            Ovb2EffectHandler::handle_registered_with_budget(
                &mut self.host,
                operation,
                callee,
                arguments,
                budget,
            )
        } else {
            self.handle_context(callee, arguments)
        }
    }

    fn handle_context_registered_with_cancellation_and_budget(
        &mut self,
        operation: &str,
        callee: &Expr,
        arguments: &[ContextValue],
        budget: &mut StepBudget,
        cancellation: Option<&CancellationToken>,
    ) -> Result<Option<ContextValue>, EvaluationError> {
        if operation == "sys.blob.capture_file" {
            Ovb2EffectHandler::handle_registered_with_cancellation_and_budget(
                &mut self.host,
                operation,
                callee,
                arguments,
                budget,
                cancellation,
            )
        } else {
            self.handle_context(callee, arguments)
        }
    }
}

#[test]
fn authored_capture_fixture_survives_nested_tuple_effect_and_row_conversion() {
    let (directory, repository, relation_id) = format3::empty_format3_repository();
    let root = directory.path();
    std::fs::write(root.join("note.wav"), b"authored capture fixture").unwrap();

    let mut filesystem = FilesystemProvider::with_limits(65_536, 16).unwrap();
    filesystem.allow_root(root).unwrap();
    let host = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(repository.capture_capability(relation_id).unwrap());

    let media_root = format!("{:?}", root.to_string_lossy().as_ref());
    let fixture = include_str!("fixtures/media/import-song-annotation.orna")
        .replace("__MEDIA_ROOT__", &media_root);
    let source = format!(
        "fn capture() = {fixture}; \
         fn relay(value: Blob) = sys.test.persist((value, \"capture.json\")); \
         fn entry() = relay(capture());"
    );
    let parsed = parse_module(&source);
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    let functions = parsed
        .value
        .items
        .iter()
        .map(|item| {
            let orna_syntax_v1::Declaration::Function { signature, body } = &item.declaration else {
                panic!("function declaration expected")
            };
            (
                signature.name.clone(),
                PureFunction {
                    parameters: signature.parameters.clone(),
                    body: body.clone(),
                    environment: Environment::new(),
                },
            )
        })
        .collect::<Functions>();
    let mut effects = FixtureEffects {
        host,
        persisted: None,
        encoded_row: None,
    };

    let result = invoke_named_with_context_effects(
        "entry",
        &functions,
        &Default::default(),
        Limits::default(),
        &mut effects,
    )
    .expect("authored capture, annotation, and tuple effect succeed");

    assert_eq!(result.format(), ValueFormat::Ovb2);
    assert_eq!(result, effects.persisted.expect("tuple reached the row effect"));
    let binding = effects.encoded_row.expect("tuple Blob reached row conversion");
    assert!(binding.windows(b"audio/mp4".len()).any(|part| part == b"audio/mp4"));
    assert!(binding.windows(b"m4b".len()).any(|part| part == b"m4b"));
}
