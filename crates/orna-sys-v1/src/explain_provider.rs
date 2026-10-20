//! Provider binding for `sys.explain(Query)`.
//!
//! The generated provider ABI declares the operation and its
//! `langitem.sys.explain@1.0` role. This module supplies the native
//! implementation that the role registry links to. Turning a typed `Query<T>`
//! value into a snapshot-pinned [`QueryPlanDescription`] requires catalogue
//! resolution, so that step is supplied by the host through
//! [`QueryPlanResolver`]. Planning and `sys.Plan` serialization are owned here.

use crate::{
    AbiVersion, EffectSet, ExplainError, FailureCode, OperationId, ProviderFailure, ProviderId,
    ProviderOffer, QueryPlanDescription, SemanticRoleId, SystemEffect, SystemOperationProvider,
    TypeId, TypedValue, explain_query,
};

/// The operation this provider implements. Must match the generated ABI name.
pub const EXPLAIN_QUERY_OPERATION: &str = "sys.explain(Query)";
/// Provider identity of the built-in system provider.
pub const BUILTIN_SYS_PROVIDER: &str = "orna.sys.v1";
/// Semantic role linked by `sys.explain(Query)`.
pub const EXPLAIN_ROLE: &str = "langitem.sys.explain";
/// Static type of the returned structured plan.
pub const SYS_PLAN_TYPE: &str = "sys.Plan";

/// Host adapter that resolves a typed `Query<T>` argument against the pinned
/// catalogue snapshot. Failures are reported with a declared-or-undeclared
/// failure code; the registry validates the code against the ABI.
pub trait QueryPlanResolver: Send + Sync {
    fn resolve(&self, query: &TypedValue) -> Result<QueryPlanDescription, FailureCode>;
}

/// Native `sys.explain(Query)` implementation linked to the explain role.
pub struct ExplainQueryProvider<R> {
    offer: ProviderOffer,
    resolver: R,
}

impl<R: QueryPlanResolver> ExplainQueryProvider<R> {
    /// The offer this provider claims: built-in `orna.sys.v1` providing
    /// `langitem.sys.explain` at version 1.0 with read effect only.
    pub fn new(resolver: R) -> Result<Self, crate::ProviderAbiError> {
        Ok(Self {
            offer: ProviderOffer {
                provider: ProviderId::new(BUILTIN_SYS_PROVIDER)?,
                role: SemanticRoleId::new(EXPLAIN_ROLE)?,
                version: AbiVersion::V1_0,
                effects: EffectSet::one(SystemEffect::Read),
            },
            resolver,
        })
    }
}

impl<R: QueryPlanResolver> SystemOperationProvider for ExplainQueryProvider<R> {
    fn offer(&self) -> &ProviderOffer {
        &self.offer
    }

    fn invoke(
        &self,
        operation: &OperationId,
        arguments: &[TypedValue],
    ) -> Result<TypedValue, ProviderFailure> {
        if operation.as_str() != EXPLAIN_QUERY_OPERATION {
            return Err(failure(ExplainError::InvalidObject));
        }
        let [query] = arguments else {
            return Err(failure(ExplainError::InvalidObject));
        };
        if !query.static_type().as_str().starts_with("Query<") {
            return Err(failure(ExplainError::InvalidObject));
        }
        let description = self
            .resolver
            .resolve(query)
            .map_err(|code| ProviderFailure {
                code,
                payload: None,
            })?;
        let explained = explain_query(&description).map_err(failure)?;
        let bytes = serde_json::to_vec(explained.plan())
            .map_err(|_| failure(ExplainError::InvalidPlanTree))?;
        Ok(TypedValue::public(TypeId::new(SYS_PLAN_TYPE), bytes))
    }
}

fn failure(error: ExplainError) -> ProviderFailure {
    ProviderFailure {
        code: FailureCode::new(error.code()).expect("explain error codes are valid failure codes"),
        payload: None,
    }
}
