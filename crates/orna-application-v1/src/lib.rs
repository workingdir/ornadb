//! The authoritative application boundary for Orna 1.0.
//!
//! This crate deliberately keeps orchestration small: syntax parsing and the
//! semantic resolver/type checker remain in their owning crates, while this
//! boundary is the one place an application source becomes an admitted
//! evaluator program. Runtime table work is staged against the exact captured
//! activation context and is never published by this crate.

use std::{fmt, sync::Arc, time::UNIX_EPOCH};
use orna_evaluator_v1::{Environment, EvaluationError, Functions, Limits, PureFunction, invoke_named};
use orna_foundation_v1::CanonicalValue;
use orna_runtime_v1::{NoFault, RuntimeActivationContext, RuntimeError, StagedTableActivation, TableMutation};
use orna_semantic_v1::{Catalogue, ModuleInput, analyze_with_catalogue};
use orna_syntax_v1::{Declaration, parse_module_with_file};
use sha2::{Digest, Sha256};

const DIGEST_DOMAIN: &[u8] = b"ORNA-ACTIVATION-DIGEST\0";

/// Errors raised before an application is allowed to execute.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApplicationError {
    /// The frozen syntax parser rejected the source.
    Parse(Vec<String>),
    /// Name resolution or static type/effect checking rejected the source.
    Semantic(Vec<String>),
    /// The requested entry function was not present in the admitted module.
    MissingEntry(String),
    /// The bounded evaluator rejected the already-admitted program.
    Evaluation(String),
    /// Runtime staging rejected a malformed mutation or activation.
    Runtime(String),
    /// Canonical snapshot encoding failed while computing an activation digest.
    DigestEncoding,
}

impl fmt::Display for ApplicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(_) => formatter.write_str("application source failed syntax admission"),
            Self::Semantic(_) => formatter.write_str("application source failed semantic admission"),
            Self::MissingEntry(name) => write!(formatter, "application entry `{name}` was not admitted"),
            Self::Evaluation(code) => write!(formatter, "application evaluation failed: {code}"),
            Self::Runtime(message) => write!(formatter, "runtime activation staging failed: {message}"),
            Self::DigestEncoding => formatter.write_str("activation context could not be canonically encoded"),
        }
    }
}

impl std::error::Error for ApplicationError {}

/// A source-backed application authority. The catalogue and evaluator limits
/// are captured once so every admission in this authority uses the same
/// resolver and bounded execution policy.
#[derive(Clone, Debug)]
pub struct ApplicationAuthority {
    catalogue: Catalogue,
    limits: Limits,
}

impl ApplicationAuthority {
    /// Creates an authority with an explicit declaration catalogue and limits.
    #[must_use]
    pub fn new(catalogue: Catalogue, limits: Limits) -> Self {
        Self { catalogue, limits }
    }

    /// Admits one source module through parser, resolver and type/effect checks.
    /// No evaluator function is exposed until all three stages succeed.
    pub fn admit_module(
        &self,
        logical_path: impl Into<String>,
        source: impl Into<String>,
        entry: impl Into<String>,
    ) -> Result<AdmittedApplication, ApplicationError> {
        let logical_path = logical_path.into();
        let source = source.into();
        let entry = entry.into();
        self.limits
            .check_source(&source)
            .map_err(|error| ApplicationError::Evaluation(error.code().to_owned()))?;

        let parsed = parse_module_with_file(&source, logical_path.clone());
        if !parsed.is_ok() {
            return Err(ApplicationError::Parse(
                parsed
                    .diagnostics
                    .iter()
                    .map(|diagnostic| diagnostic.code.to_owned())
                    .collect(),
            ));
        }

        let analysis = analyze_with_catalogue(
            &[ModuleInput::new(logical_path.clone(), source.clone())],
            &self.catalogue,
        );
        if !analysis.is_ok() {
            return Err(ApplicationError::Semantic(
                analysis
                    .diagnostics
                    .iter()
                    .map(|diagnostic| diagnostic.code().to_owned())
                    .collect(),
            ));
        }

        let mut functions = Functions::new();
        for item in &parsed.value.items {
            if let Declaration::Function { signature, body } = &item.declaration {
                functions.insert(
                    signature.name.clone(),
                    PureFunction {
                        parameters: signature.parameters.clone(),
                        body: body.clone(),
                        environment: Environment::new(),
                    },
                );
            }
        }
        if !functions.contains_key(&entry) {
            return Err(ApplicationError::MissingEntry(entry));
        }

        Ok(AdmittedApplication {
            logical_path,
            source_digest: Sha256::digest(source.as_bytes()).into(),
            entry,
            functions,
            limits: self.limits,
        })
    }

    /// Executes the admitted entry through the production bounded evaluator.
    pub fn evaluate(
        &self,
        application: &AdmittedApplication,
        arguments: &Environment,
    ) -> Result<CanonicalValue, ApplicationError> {
        invoke_named(
            &application.entry,
            &application.functions,
            arguments,
            application.limits,
        )
        .map_err(|error: EvaluationError| ApplicationError::Evaluation(error.code().to_owned()))
    }

    /// Computes the canonical digest for staged table work in one captured
    /// runtime activation. The context pin and ordered mutation bytes are both
    /// included, so a digest cannot be reused across CWD generations.
    pub fn canonical_digest(
        context: &RuntimeActivationContext,
        mutations: &[TableMutation],
    ) -> Result<[u8; 32], ApplicationError> {
        let snapshot = context
            .capture()
            .snapshot()
            .encode()
            .map_err(|_| ApplicationError::DigestEncoding)?;
        let mut digest = Sha256::new();
        digest.update(DIGEST_DOMAIN);
        put_bytes(&mut digest, &snapshot);
        digest.update(context.capture().generation_digest());
        let elapsed = context
            .activation_time()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        digest.update(elapsed.as_secs().to_be_bytes());
        digest.update(elapsed.subsec_nanos().to_be_bytes());
        for mutation in mutations {
            digest.update(mutation.id());
            put_bytes(&mut digest, mutation.table().as_bytes());
            put_bytes(&mut digest, mutation.key());
            match mutation.value() {
                Some(value) => {
                    digest.update([1]);
                    put_bytes(&mut digest, value);
                }
                None => digest.update([0]),
            }
        }
        Ok(digest.finalize().into())
    }

    /// Stages a typed mutation for a captured activation. Publication remains
    /// the runtime's owner-fenced commit operation.
    pub fn stage_mutation(
        &self,
        context: RuntimeActivationContext,
        mutation: TableMutation,
    ) -> Result<StagedTableActivation, ApplicationError> {
        let digest = Self::canonical_digest(&context, std::slice::from_ref(&mutation))?;
        StagedTableActivation::from_source(
            context,
            vec![mutation],
            digest,
            Arc::new(NoFault),
        )
        .map_err(|error: RuntimeError| ApplicationError::Runtime(error.to_string()))
    }
}

/// An immutable, fully admitted application program.
#[derive(Clone, Debug)]
pub struct AdmittedApplication {
    logical_path: String,
    source_digest: [u8; 32],
    entry: String,
    functions: Functions,
    limits: Limits,
}

impl AdmittedApplication {
    #[must_use]
    pub fn logical_path(&self) -> &str {
        &self.logical_path
    }

    #[must_use]
    pub fn source_digest(&self) -> [u8; 32] {
        self.source_digest
    }

    #[must_use]
    pub fn entry(&self) -> &str {
        &self.entry
    }
}

fn put_bytes(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_admission_reaches_real_evaluator() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let admitted = authority
            .admit_module("main.orna", "pub fn main(): Int = 41;", "main")
            .expect("source should be admitted");
        let result = authority
            .evaluate(&admitted, &Environment::new())
            .expect("entry should execute");
        let expected = orna_foundation_v1::Value::new(orna_foundation_v1::OvbRaw::Int(
            num_bigint::BigInt::from(41_i64),
        ))
        .expect("canonical integer");
        assert_eq!(result, expected);
    }
}
