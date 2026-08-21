//! Closed USER state cell model.
//!
//! This module models the durable per-principal USER state cells defined by
//! work ADR 0061 (spec ADR 0007). The logical key, the revision arithmetic,
//! and the validation facts live here as pure, backend-independent types;
//! the canonical ORV5 value encoding and the durable relation are later
//! boundary slices.
//!
//! The model is closed in two senses. Every admitted key is a legal durable
//! key: each TEXT component round-trips through the relation's TEXT columns,
//! so a component cannot contain a NUL byte. And every write is checked:
//! `apply_change` enforces the principal, type, and revision rules and fails
//! closed with the spec error codes ORNA0901 (type incompatible), ORNA0902
//! (revision conflict), and ORNA0903 (principal spoof attempt) instead of
//! returning or storing a stale value.
//!
//! Defaults: an empty `state_profile` is the default state profile and an
//! empty `instance_key` is the default function instance key. Both are
//! ordinary TEXT values; the model never rewrites them.
//!
//! Revisions: a cell that does not exist yet has current revision 0. A write
//! with `expected_revision: None` requires the cell not to exist (first
//! write only); `Some(r)` requires `r` to equal the current revision. A
//! matching write stores the value at revision `current + 1`. Every write
//! therefore carries an expectation and fails closed on a conflict, per the
//! ADR's write rule.
//!
//! The model carries the typed value as [`RuntimeValue`]. Encoding to the
//! canonical ORV5 byte form happens at the protocol boundary, exactly like
//! the rest of orna-core; this module imports neither `orna-protocol` nor
//! `orna-standard`. `updated_at` is stamped by the boundary when a write is
//! persisted; the pure model computes only the revision.

use std::{error::Error, fmt, time::SystemTime};

use crate::{
    FunctionId, PrincipalId, StateSlotId, TypeId,
    types::{ResolvedType, TypeDescriptor, TypeDescriptorKind},
    value::{ConstructedValueKind, RuntimeType, RuntimeValue},
};

/// Rejects one TEXT key component that cannot round-trip through the durable
/// relation. An empty component is legal: it is the default profile or the
/// default instance key.
fn validate_state_text(component: &str, what: &str) -> Result<(), UserStateError> {
    if component.contains('\0') {
        return Err(UserStateError::InvalidKey {
            reason: format!("{what} must not contain a NUL byte"),
        });
    }
    Ok(())
}

/// Returns whether a type identity is one of the transient sealed Inspector
/// carriers. The identities are the existing sealed system registry entries;
/// USER state must never turn any of them into durable value type identity.
fn is_sealed_inspect_type_id(type_id: TypeId) -> bool {
    matches!(
        type_id,
        crate::system::SYS_INSPECT_INVOCATION_TYPE_ID
            | crate::system::SYS_INSPECT_SNAPSHOT_TYPE_ID
            | crate::system::SYS_INSPECT_SNAPSHOT_OPTIONS_TYPE_ID
            | crate::system::SYS_INSPECT_TRACE_EVENT_TYPE_ID
            | crate::system::SYS_INSPECT_INVOCATION_NODES_TYPE_ID
            | crate::system::SYS_INSPECT_CALLS_TYPE_ID
            | crate::system::SYS_INSPECT_RESOURCES_TYPE_ID
            | crate::system::SYS_INSPECT_STATE_CELLS_TYPE_ID
            | crate::system::SYS_INSPECT_UI_NODES_TYPE_ID
            | crate::system::SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID
            | crate::system::SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID
            | crate::system::SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID
    )
}

fn descriptor_contains_sealed_inspect_type(descriptor: &TypeDescriptor) -> bool {
    match descriptor.kind() {
        TypeDescriptorKind::Named(type_id) | TypeDescriptorKind::Reference(type_id) => {
            is_sealed_inspect_type_id(type_id)
        }
        TypeDescriptorKind::List(child)
        | TypeDescriptorKind::Set(child)
        | TypeDescriptorKind::Option(child)
        | TypeDescriptorKind::Stream(child) => descriptor_contains_sealed_inspect_type(child),
        TypeDescriptorKind::Map { key, value } => {
            descriptor_contains_sealed_inspect_type(key)
                || descriptor_contains_sealed_inspect_type(value)
        }
    }
}

/// Returns whether a runtime value contains a sealed Inspector identity.
///
/// USER state stores typed runtime values, so checking only the separately
/// supplied metadata type is insufficient. The check walks nested records,
/// constructed values, and descriptor-bearing invocation offer metadata before a
/// change is accepted or a recovered cell is exposed to the client.
pub fn is_sealed_inspect_runtime_value(value: &RuntimeValue) -> bool {
    let type_is_sealed = match value.runtime_type() {
        RuntimeType::Flat(ResolvedType::Named(type_id))
        | RuntimeType::Flat(ResolvedType::Reference { target: type_id })
        | RuntimeType::Flat(ResolvedType::Value(type_id)) => is_sealed_inspect_type_id(type_id),
        RuntimeType::Flat(ResolvedType::Scalar(_)) => false,
        RuntimeType::Constructed(descriptor) => {
            descriptor_contains_sealed_inspect_type(descriptor)
        }
    };
    if type_is_sealed {
        return true;
    }

    match value {
        RuntimeValue::Record(record) => record
            .fields()
            .iter()
            .any(is_sealed_inspect_runtime_value),
        RuntimeValue::Constructed(constructed) => match constructed.kind() {
            ConstructedValueKind::Option(value) => {
                value.is_some_and(is_sealed_inspect_runtime_value)
            }
            ConstructedValueKind::List(values) | ConstructedValueKind::Set(values) => values
                .iter()
                .any(is_sealed_inspect_runtime_value),
            ConstructedValueKind::Map(entries) => entries.iter().any(|(key, value)| {
                is_sealed_inspect_runtime_value(key) || is_sealed_inspect_runtime_value(value)
            }),
        },
        RuntimeValue::InvokeValue(invoke_value) => {
            is_sealed_inspect_runtime_value(invoke_value.value())
        }
        RuntimeValue::InvokeRequest(request) => {
            request
                .arguments()
                .iter()
                .any(|argument| is_sealed_inspect_runtime_value(argument.value().value()))
                || request
                    .caller_context()
                    .preference_policy()
                    .is_some_and(|value| is_sealed_inspect_runtime_value(value.value()))
                || request
                    .client_offer()
                    .limits()
                    .is_some_and(|value| is_sealed_inspect_runtime_value(value.value()))
                || request
                    .client_offer()
                    .preferences()
                    .is_some_and(|value| is_sealed_inspect_runtime_value(value.value()))
                || request.client_offer().sink_offers().iter().any(|offer| {
                    descriptor_contains_sealed_inspect_type(offer.descriptor())
                        || offer
                            .limits()
                            .is_some_and(|value| is_sealed_inspect_runtime_value(value.value()))
                })
                || request.client_offer().runtime_offers().iter().any(|offer| {
                    offer
                        .consumed_descriptors()
                        .iter()
                        .any(descriptor_contains_sealed_inspect_type)
                        || offer
                            .limits()
                            .is_some_and(|value| is_sealed_inspect_runtime_value(value.value()))
                })
                || request
                    .observer_context()
                    .is_some_and(|value| is_sealed_inspect_runtime_value(value.value()))
        }
        RuntimeValue::InvokeEvent(event) => match event.body() {
            crate::invocation::InvocationEventBody::ValueBatch { schema, values } => {
                schema
                    .as_ref()
                    .is_some_and(|value| is_sealed_inspect_runtime_value(value.value()))
                    || values
                        .iter()
                        .any(|value| is_sealed_inspect_runtime_value(value.value()))
            }
            crate::invocation::InvocationEventBody::Failed(failure) => failure
                .details()
                .is_some_and(|value| is_sealed_inspect_runtime_value(value.value())),
            _ => false,
        },
        _ => false,
    }
}

fn reject_sealed_inspect_value(value: &RuntimeValue) -> Result<(), UserStateError> {
    if is_sealed_inspect_runtime_value(value) {
        return Err(UserStateError::InvalidChange {
            reason: "sealed Inspector values cannot be persisted in USER state".to_owned(),
        });
    }
    Ok(())
}

fn reject_sealed_inspect_type_id(type_id: TypeId) -> Result<(), UserStateError> {
    if is_sealed_inspect_type_id(type_id) {
        return Err(UserStateError::InvalidChange {
            reason: format!("sealed Inspector type {type_id} cannot be persisted in USER state"),
        });
    }
    Ok(())
}

/// The logical key of one USER state cell without the session principal.
///
/// This is the identity a write result carries: a change cannot name a
/// principal, so a result identifies the affected cell by exactly the
/// components the change did carry.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct UserStateKeyWithoutPrincipal {
    root_function: FunctionId,
    state_profile: String,
    function: FunctionId,
    instance_key: String,
    state_slot: StateSlotId,
}

impl UserStateKeyWithoutPrincipal {
    /// Creates one key from its exact logical components.
    ///
    /// An empty `state_profile` is the default profile and an empty
    /// `instance_key` is the default instance key; both are accepted. A
    /// component containing a NUL byte cannot round-trip through the
    /// relation's TEXT columns and is rejected.
    pub fn new(
        root_function: FunctionId,
        state_profile: String,
        function: FunctionId,
        instance_key: String,
        state_slot: StateSlotId,
    ) -> Result<Self, UserStateError> {
        validate_state_text(&state_profile, "state profile")?;
        validate_state_text(&instance_key, "instance key")?;
        Ok(Self {
            root_function,
            state_profile,
            function,
            instance_key,
            state_slot,
        })
    }

    /// Returns the root function whose invocation owns this cell.
    pub const fn root_function(&self) -> FunctionId {
        self.root_function
    }

    /// Returns the state profile; the empty string is the default profile.
    pub fn state_profile(&self) -> &str {
        &self.state_profile
    }

    /// Returns the function that owns the state slot.
    pub const fn function(&self) -> FunctionId {
        self.function
    }

    /// Returns the function-instance identity; the empty string is the
    /// default instance.
    pub fn instance_key(&self) -> &str {
        &self.instance_key
    }

    /// Returns the stable state-slot identity.
    pub const fn state_slot(&self) -> StateSlotId {
        self.state_slot
    }

    /// Adds the authenticated session principal, completing the full
    /// logical key of the durable cell.
    pub fn with_principal(self, principal: PrincipalId) -> UserStateKey {
        UserStateKey {
            principal,
            root_function: self.root_function,
            state_profile: self.state_profile,
            function: self.function,
            instance_key: self.instance_key,
            state_slot: self.state_slot,
        }
    }
}

impl fmt::Display for UserStateKeyWithoutPrincipal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "root_function={} state_profile={:?} function={} instance_key={:?} state_slot={}",
            self.root_function,
            self.state_profile,
            self.function,
            self.instance_key,
            self.state_slot
        )
    }
}

/// The full logical key of one durable USER state cell.
///
/// The principal comes from the authenticated session and never from a
/// request; the remaining components identify one cell inside that
/// principal's state. Changing any component addresses a distinct cell.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct UserStateKey {
    principal: PrincipalId,
    root_function: FunctionId,
    state_profile: String,
    function: FunctionId,
    instance_key: String,
    state_slot: StateSlotId,
}

impl UserStateKey {
    /// Creates one full logical key from its exact components.
    ///
    /// See [`UserStateKeyWithoutPrincipal::new`] for the TEXT-component
    /// rules: empty profile and instance key are the defaults and are
    /// accepted; NUL bytes are rejected.
    pub fn new(
        principal: PrincipalId,
        root_function: FunctionId,
        state_profile: String,
        function: FunctionId,
        instance_key: String,
        state_slot: StateSlotId,
    ) -> Result<Self, UserStateError> {
        validate_state_text(&state_profile, "state profile")?;
        validate_state_text(&instance_key, "instance key")?;
        Ok(Self {
            principal,
            root_function,
            state_profile,
            function,
            instance_key,
            state_slot,
        })
    }

    /// Returns the authenticated session principal that owns this cell.
    pub const fn principal(&self) -> PrincipalId {
        self.principal
    }

    /// Returns the root function whose invocation owns this cell.
    pub const fn root_function(&self) -> FunctionId {
        self.root_function
    }

    /// Returns the state profile; the empty string is the default profile.
    pub fn state_profile(&self) -> &str {
        &self.state_profile
    }

    /// Returns the function that owns the state slot.
    pub const fn function(&self) -> FunctionId {
        self.function
    }

    /// Returns the function-instance identity; the empty string is the
    /// default instance.
    pub fn instance_key(&self) -> &str {
        &self.instance_key
    }

    /// Returns the stable state-slot identity.
    pub const fn state_slot(&self) -> StateSlotId {
        self.state_slot
    }

    /// Returns this key without the session principal.
    pub fn without_principal(&self) -> UserStateKeyWithoutPrincipal {
        UserStateKeyWithoutPrincipal {
            root_function: self.root_function,
            state_profile: self.state_profile.clone(),
            function: self.function,
            instance_key: self.instance_key.clone(),
            state_slot: self.state_slot,
        }
    }
}

impl fmt::Display for UserStateKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "principal={} {}",
            self.principal,
            self.without_principal()
        )
    }
}

/// One durable USER state cell.
///
/// `value_type` is the persisted type of the typed value. Consistency
/// between the decoded [`RuntimeValue`] and `value_type` is established by
/// the canonical typed codec at the boundary; this model treats `value_type`
/// as the authoritative compatibility fact. `updated_at` is the boundary
/// write time.
#[derive(Clone, Debug, PartialEq)]
pub struct UserStateCell {
    key: UserStateKey,
    value: RuntimeValue,
    value_type: TypeId,
    revision: u64,
    updated_at: SystemTime,
}

impl UserStateCell {
    /// Recovers one cell from its exact durable facts.
    ///
    /// The key is already validated by [`UserStateKey::new`], so this
    /// constructor is infallible.
    pub fn new(
        key: UserStateKey,
        value: RuntimeValue,
        value_type: TypeId,
        revision: u64,
        updated_at: SystemTime,
    ) -> Self {
        Self {
            key,
            value,
            value_type,
            revision,
            updated_at,
        }
    }

    /// Returns the full logical key of this cell.
    pub const fn key(&self) -> &UserStateKey {
        &self.key
    }

    /// Returns the typed runtime value stored in this cell.
    pub const fn value(&self) -> &RuntimeValue {
        &self.value
    }

    /// Returns the persisted type of the stored value.
    pub const fn value_type(&self) -> TypeId {
        self.value_type
    }

    /// Returns the monotonic revision of this cell.
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Returns the boundary write time of this cell.
    pub const fn updated_at(&self) -> SystemTime {
        self.updated_at
    }
}

/// One write input for a USER state cell.
///
/// A change never carries a principal: the session principal is derived by
/// the server, so the request cannot choose another principal. The value is
/// typed: the change carries the runtime value and its type together.
#[derive(Clone, Debug, PartialEq)]
pub struct UserStateChange {
    root_function: FunctionId,
    state_profile: String,
    function: FunctionId,
    instance_key: String,
    state_slot: StateSlotId,
    expected_revision: Option<u64>,
    value: RuntimeValue,
    value_type: TypeId,
}

impl UserStateChange {
    /// Creates one change from its exact components.
    ///
    /// `expected_revision: None` requires the target cell not to exist
    /// (first write only); `Some(r)` requires the current revision to equal
    /// `r`. The TEXT components follow the key rules: empty profile and
    /// instance key are the defaults and are accepted; NUL bytes are
    /// rejected.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        root_function: FunctionId,
        state_profile: String,
        function: FunctionId,
        instance_key: String,
        state_slot: StateSlotId,
        expected_revision: Option<u64>,
        value: RuntimeValue,
        value_type: TypeId,
    ) -> Result<Self, UserStateError> {
        validate_state_text(&state_profile, "state profile")?;
        validate_state_text(&instance_key, "instance key")?;
        reject_sealed_inspect_type_id(value_type)?;
        reject_sealed_inspect_value(&value)?;
        Ok(Self {
            root_function,
            state_profile,
            function,
            instance_key,
            state_slot,
            expected_revision,
            value,
            value_type,
        })
    }

    /// Returns the root function whose invocation owns the target cell.
    pub const fn root_function(&self) -> FunctionId {
        self.root_function
    }

    /// Returns the state profile; the empty string is the default profile.
    pub fn state_profile(&self) -> &str {
        &self.state_profile
    }

    /// Returns the function that owns the state slot.
    pub const fn function(&self) -> FunctionId {
        self.function
    }

    /// Returns the function-instance identity; the empty string is the
    /// default instance.
    pub fn instance_key(&self) -> &str {
        &self.instance_key
    }

    /// Returns the stable state-slot identity.
    pub const fn state_slot(&self) -> StateSlotId {
        self.state_slot
    }

    /// Returns the expected current revision; `None` requires the cell not
    /// to exist.
    pub const fn expected_revision(&self) -> Option<u64> {
        self.expected_revision
    }

    /// Returns the typed runtime value to store.
    pub const fn value(&self) -> &RuntimeValue {
        &self.value
    }

    /// Returns the type of the value to store.
    pub const fn value_type(&self) -> TypeId {
        self.value_type
    }

    /// Returns the affected cell key without the session principal.
    pub fn key_without_principal(&self) -> UserStateKeyWithoutPrincipal {
        UserStateKeyWithoutPrincipal {
            root_function: self.root_function,
            state_profile: self.state_profile.clone(),
            function: self.function,
            instance_key: self.instance_key.clone(),
            state_slot: self.state_slot,
        }
    }
}

/// The closed outcome of applying one change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UserStateWriteOutcome {
    /// The change wrote the cell at the returned new revision.
    Written {
        /// The revision stored by the write.
        revision: u64,
    },
    /// The change failed closed with ORNA0902. The returned revision is the
    /// cell's current revision so the client can reconcile.
    Conflict {
        /// The cell's current revision at the time of the write attempt.
        current_revision: u64,
    },
}

/// One closed write result, aligned with its change by the carried key.
///
/// Results are produced in the same order as the changes they correspond
/// to; the carried key identifies the affected cell without the session
/// principal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserStateWriteResult {
    key: UserStateKeyWithoutPrincipal,
    outcome: UserStateWriteOutcome,
}

impl UserStateWriteResult {
    /// Creates one write result for one change.
    pub const fn new(key: UserStateKeyWithoutPrincipal, outcome: UserStateWriteOutcome) -> Self {
        Self { key, outcome }
    }

    /// Returns the affected cell key without the session principal.
    pub const fn key(&self) -> &UserStateKeyWithoutPrincipal {
        &self.key
    }

    /// Returns the closed write outcome.
    pub const fn outcome(&self) -> UserStateWriteOutcome {
        self.outcome
    }
}

/// A closed USER state failure.
///
/// The spec-flavoured variants map to the stable spec error codes ORNA0901
/// (type incompatible), ORNA0902 (revision conflict), and ORNA0903 (state
/// principal spoof attempt). The remaining variants are model-shape errors
/// that never map to a client spec code.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UserStateError {
    /// The change's value type differs from the cell's persisted value type
    /// (ORNA0901). Fails closed rather than returning or storing a value in
    /// a type the current slot no longer declares.
    TypeIncompatible {
        /// The affected cell key without the session principal.
        key: Box<UserStateKeyWithoutPrincipal>,
        /// The type the change carries.
        expected: TypeId,
        /// The type persisted with the current cell value.
        current: TypeId,
    },
    /// The change's expected revision does not match the cell's current
    /// revision (ORNA0902). The current revision is returned so the client
    /// can reconcile.
    RevisionConflict {
        /// The affected cell key without the session principal.
        key: Box<UserStateKeyWithoutPrincipal>,
        /// The expectation the change carried; `None` means the change
        /// required the cell not to exist.
        expected: Option<u64>,
        /// The cell's current revision at the time of the write attempt.
        current: u64,
    },
    /// A cell belonging to another principal reached the session-principal
    /// write path (ORNA0903). A change cannot carry a principal, so this is
    /// only reachable through a boundary that confused principals.
    PrincipalSpoofAttempt {
        /// The principal that owns the supplied cell.
        cell_principal: PrincipalId,
        /// The authenticated session principal of the write.
        session_principal: PrincipalId,
    },
    /// A key component cannot round-trip through the durable relation.
    InvalidKey {
        /// The violated key rule.
        reason: String,
    },
    /// A change or cell state violates a closed model invariant.
    InvalidChange {
        /// The violated model rule.
        reason: String,
    },
}

impl UserStateError {
    /// Returns the stable spec error code for spec-flavoured failures, and
    /// `None` for model-shape failures that never map to a client code.
    pub const fn code(&self) -> Option<&'static str> {
        match self {
            Self::TypeIncompatible { .. } => Some("ORNA0901"),
            Self::RevisionConflict { .. } => Some("ORNA0902"),
            Self::PrincipalSpoofAttempt { .. } => Some("ORNA0903"),
            Self::InvalidKey { .. } | Self::InvalidChange { .. } => None,
        }
    }
}

impl fmt::Display for UserStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TypeIncompatible {
                key,
                expected,
                current,
            } => write!(
                formatter,
                "USER state type incompatible with current cell: change type {expected} does not \
                 match cell type {current} for {key} (ORNA0901)"
            ),
            Self::RevisionConflict {
                key,
                expected,
                current,
            } => write!(
                formatter,
                "USER state revision conflict for {key}: expected {expected:?}, current {current} \
                 (ORNA0902)"
            ),
            Self::PrincipalSpoofAttempt {
                cell_principal,
                session_principal,
            } => write!(
                formatter,
                "state principal spoof attempt rejected: cell principal {cell_principal} does not \
                 match session principal {session_principal} (ORNA0903)"
            ),
            Self::InvalidKey { reason } => {
                write!(formatter, "invalid USER state key: {reason}")
            }
            Self::InvalidChange { reason } => {
                write!(formatter, "invalid USER state change: {reason}")
            }
        }
    }
}

impl Error for UserStateError {}

/// Applies one change under the session principal.
///
/// `cell` is the current durable cell for the change's key, or `None` when
/// no cell exists yet (its current revision is 0). The rules run in order:
///
/// 1. **Principal**: a supplied cell must belong to the session principal;
///    a cell of another principal is a cross-principal access attempt and
///    fails closed with ORNA0903. A change carries no principal by
///    construction, so this is the only principal check the model needs.
/// 2. **Key**: the supplied cell must be the cell for the change's key;
///    otherwise the boundary handed over a mismatched cell and the model
///    fails closed as an invalid change.
/// 3. **Type**: when a cell exists, `change.value_type` must equal the
///    cell's persisted `value_type`; otherwise ORNA0901.
/// 4. **Revision**: `expected_revision == None` requires the cell not to
///    exist (first write only); `Some(r)` requires `r` to equal the current
///    revision, which is 0 for a missing cell. Any other expectation fails
///    closed with ORNA0902 and returns the current revision. A matching
///    write stores the value at revision `current + 1`.
///
/// The result carries the change's key without the session principal; the
/// boundary stamps `updated_at` when it persists the write.
pub fn apply_change(
    cell: Option<&UserStateCell>,
    change: &UserStateChange,
    principal: PrincipalId,
) -> Result<UserStateWriteResult, UserStateError> {
    let key = change.key_without_principal();
    let Some(cell) = cell else {
        reject_sealed_inspect_type_id(change.value_type())?;
        reject_sealed_inspect_value(change.value())?;
        let outcome = match change.expected_revision() {
            None | Some(0) => UserStateWriteOutcome::Written { revision: 1 },
            Some(expected) => {
                return Err(UserStateError::RevisionConflict {
                    key: Box::new(key),
                    expected: Some(expected),
                    current: 0,
                });
            }
        };
        return Ok(UserStateWriteResult::new(key, outcome));
    };

    if cell.key().principal() != principal {
        return Err(UserStateError::PrincipalSpoofAttempt {
            cell_principal: cell.key().principal(),
            session_principal: principal,
        });
    }
    if cell.key().without_principal() != key {
        return Err(UserStateError::InvalidChange {
            reason: format!(
                "the supplied cell key does not match the change key {key}: {}",
                cell.key().without_principal()
            ),
        });
    }
    reject_sealed_inspect_type_id(change.value_type())?;
    reject_sealed_inspect_value(change.value())?;
    reject_sealed_inspect_type_id(cell.value_type())?;
    reject_sealed_inspect_value(cell.value())?;
    if change.value_type() != cell.value_type() {
        return Err(UserStateError::TypeIncompatible {
            key: Box::new(key),
            expected: change.value_type(),
            current: cell.value_type(),
        });
    }

    let current = cell.revision();
    let outcome = match change.expected_revision() {
        None => {
            return Err(UserStateError::RevisionConflict {
                key: Box::new(key),
                expected: None,
                current,
            });
        }
        Some(expected) if expected != current => {
            return Err(UserStateError::RevisionConflict {
                key: Box::new(key),
                expected: Some(expected),
                current,
            });
        }
        Some(_) => {
            let Some(next) = current.checked_add(1) else {
                return Err(UserStateError::InvalidChange {
                    reason: format!("revision counter overflow at {current} for {key}"),
                });
            };
            UserStateWriteOutcome::Written { revision: next }
        }
    };
    Ok(UserStateWriteResult::new(key, outcome))
}

/// Reports whether a recovered cell's persisted value type still matches the
/// declared type of its state slot.
///
/// This is the load-time ORNA0901 check: a cell whose type no longer matches
/// the slot's declared type fails closed instead of returning a stale value.
pub fn cell_type_matches(cell: &UserStateCell, declared_type: TypeId) -> bool {
    !is_sealed_inspect_type_id(cell.value_type())
        && !is_sealed_inspect_runtime_value(cell.value())
        && cell.value_type() == declared_type
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invocation::{
        InvocationArgument, InvocationCallerContext, InvocationCallerKind, InvocationClientOffer,
        InvocationEventBody, InvocationParameterSelector, InvocationRuntimeOffer,
        InvocationSinkOffer, InvocationTarget, InvocationTracePolicy, InvokeEvent, InvokeRequest,
        InvokeRequestInput, InvokeValue,
    };
    const PRINCIPAL_A: u8 = 0x11;
    const PRINCIPAL_B: u8 = 0x22;
    const ROOT: u8 = 0x33;
    const FUNCTION: u8 = 0x44;
    const SLOT: u8 = 0x55;
    const TYPE_INT: u8 = 0x66;
    const TYPE_TEXT: u8 = 0x77;
    fn principal_id(byte: u8) -> PrincipalId {
        PrincipalId::from_bytes([byte; 16])
    }

    fn function_id(byte: u8) -> FunctionId {
        FunctionId::from_bytes([byte; 16])
    }

    fn state_slot_id(byte: u8) -> StateSlotId {
        StateSlotId::from_bytes([byte; 16])
    }

    fn type_id(byte: u8) -> TypeId {
        TypeId::from_bytes([byte; 16])
    }

    fn key(
        principal_byte: u8,
        root_byte: u8,
        profile: &str,
        function_byte: u8,
        instance: &str,
        slot_byte: u8,
    ) -> UserStateKey {
        UserStateKey::new(
            principal_id(principal_byte),
            function_id(root_byte),
            profile.to_owned(),
            function_id(function_byte),
            instance.to_owned(),
            state_slot_id(slot_byte),
        )
        .expect("a valid test key")
    }

    fn default_key() -> UserStateKey {
        key(PRINCIPAL_A, ROOT, "", FUNCTION, "", SLOT)
    }

    fn key_without(
        root_byte: u8,
        profile: &str,
        function_byte: u8,
        instance: &str,
        slot_byte: u8,
    ) -> UserStateKeyWithoutPrincipal {
        UserStateKeyWithoutPrincipal::new(
            function_id(root_byte),
            profile.to_owned(),
            function_id(function_byte),
            instance.to_owned(),
            state_slot_id(slot_byte),
        )
        .expect("a valid test key")
    }

    #[allow(clippy::too_many_arguments)]
    fn change(
        root_byte: u8,
        profile: &str,
        function_byte: u8,
        instance: &str,
        slot_byte: u8,
        expected_revision: Option<u64>,
        value: RuntimeValue,
        value_type: TypeId,
    ) -> UserStateChange {
        UserStateChange::new(
            function_id(root_byte),
            profile.to_owned(),
            function_id(function_byte),
            instance.to_owned(),
            state_slot_id(slot_byte),
            expected_revision,
            value,
            value_type,
        )
        .expect("a valid test change")
    }

    fn default_change(expected_revision: Option<u64>, value: RuntimeValue) -> UserStateChange {
        change(
            ROOT,
            "",
            FUNCTION,
            "",
            SLOT,
            expected_revision,
            value,
            type_id(TYPE_INT),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn cell(
        principal_byte: u8,
        root_byte: u8,
        profile: &str,
        function_byte: u8,
        instance: &str,
        slot_byte: u8,
        value_type: TypeId,
        revision: u64,
    ) -> UserStateCell {
        UserStateCell::new(
            key(
                principal_byte,
                root_byte,
                profile,
                function_byte,
                instance,
                slot_byte,
            ),
            RuntimeValue::Integer(7),
            value_type,
            revision,
            SystemTime::UNIX_EPOCH,
        )
    }

    fn default_cell(revision: u64) -> UserStateCell {
        cell(
            PRINCIPAL_A,
            ROOT,
            "",
            FUNCTION,
            "",
            SLOT,
            type_id(TYPE_INT),
            revision,
        )
    }

    fn invoke_request_with_offers(
        sink_offers: Vec<InvocationSinkOffer>,
        runtime_offers: Vec<InvocationRuntimeOffer>,
    ) -> RuntimeValue {
        RuntimeValue::InvokeRequest(
            InvokeRequest::new(InvokeRequestInput {
                target: InvocationTarget::function_id(function_id(FUNCTION)),
                arguments: vec![],
                caller_context: InvocationCallerContext::new(
                    InvocationCallerKind::CliPipe,
                    false,
                    false,
                    None,
                    None,
                    "en-GB",
                    "Europe/London",
                    None,
                )
                .expect("a valid caller context"),
                client_offer: InvocationClientOffer::new(
                    5,
                    "en-GB",
                    "Europe/London",
                    sink_offers,
                    runtime_offers,
                    1_024,
                    0,
                    None,
                    None,
                )
                .expect("a valid client offer"),
                output_requirement: None,
                state_profile: None,
                trace_policy: InvocationTracePolicy::Off,
                idempotency_key: None,
                parent_invocation_id: None,
                observer_context: None,
            })
            .expect("a valid invocation request"),
        )
    }

    fn assert_rejected_sealed_offer_value(value: RuntimeValue, message: &str) {
        let error = UserStateChange::new(
            function_id(ROOT),
            String::new(),
            function_id(FUNCTION),
            String::new(),
            state_slot_id(SLOT),
            None,
            value,
            type_id(TYPE_INT),
        )
        .expect_err(message);
        assert_eq!(
            error,
            UserStateError::InvalidChange {
                reason: "sealed Inspector values cannot be persisted in USER state".to_owned(),
            }
        );
    }

    #[test]
    fn key_construction_accepts_the_default_profile_and_default_instance() {
        let key = default_key();
        assert_eq!(key.principal(), principal_id(PRINCIPAL_A));
        assert_eq!(key.root_function(), function_id(ROOT));
        assert_eq!(key.state_profile(), "");
        assert_eq!(key.function(), function_id(FUNCTION));
        assert_eq!(key.instance_key(), "");
        assert_eq!(key.state_slot(), state_slot_id(SLOT));
    }

    #[test]
    fn key_construction_accepts_named_profile_and_instance() {
        let key = key(PRINCIPAL_A, ROOT, "dark", FUNCTION, "tab-2", SLOT);
        assert_eq!(key.state_profile(), "dark");
        assert_eq!(key.instance_key(), "tab-2");
    }

    #[test]
    fn key_construction_rejects_a_nul_in_the_state_profile() {
        let error = UserStateKey::new(
            principal_id(PRINCIPAL_A),
            function_id(ROOT),
            "dark\0mode".to_owned(),
            function_id(FUNCTION),
            String::new(),
            state_slot_id(SLOT),
        )
        .expect_err("a NUL profile cannot round-trip through TEXT");
        assert!(matches!(error, UserStateError::InvalidKey { .. }));
        assert_eq!(error.code(), None);
    }

    #[test]
    fn key_construction_rejects_a_nul_in_the_instance_key() {
        let error = UserStateKey::new(
            principal_id(PRINCIPAL_A),
            function_id(ROOT),
            String::new(),
            function_id(FUNCTION),
            "tab\0two".to_owned(),
            state_slot_id(SLOT),
        )
        .expect_err("a NUL instance key cannot round-trip through TEXT");
        assert!(matches!(error, UserStateError::InvalidKey { .. }));
    }

    #[test]
    fn change_construction_rejects_nul_components() {
        let nul_profile = UserStateChange::new(
            function_id(ROOT),
            "dark\0mode".to_owned(),
            function_id(FUNCTION),
            String::new(),
            state_slot_id(SLOT),
            Some(0),
            RuntimeValue::Integer(1),
            type_id(TYPE_INT),
        );
        assert!(matches!(
            nul_profile,
            Err(UserStateError::InvalidKey { .. })
        ));

        let nul_instance = UserStateChange::new(
            function_id(ROOT),
            String::new(),
            function_id(FUNCTION),
            "tab\0two".to_owned(),
            state_slot_id(SLOT),
            Some(0),
            RuntimeValue::Integer(1),
            type_id(TYPE_INT),
        );
        assert!(matches!(
            nul_instance,
            Err(UserStateError::InvalidKey { .. })
        ));
    }

    #[test]
    fn change_construction_rejects_every_sealed_inspector_type() {
        let sealed_types = [
            crate::system::SYS_INSPECT_INVOCATION_TYPE_ID,
            crate::system::SYS_INSPECT_SNAPSHOT_TYPE_ID,
            crate::system::SYS_INSPECT_SNAPSHOT_OPTIONS_TYPE_ID,
            crate::system::SYS_INSPECT_TRACE_EVENT_TYPE_ID,
            crate::system::SYS_INSPECT_INVOCATION_NODES_TYPE_ID,
            crate::system::SYS_INSPECT_CALLS_TYPE_ID,
            crate::system::SYS_INSPECT_RESOURCES_TYPE_ID,
            crate::system::SYS_INSPECT_STATE_CELLS_TYPE_ID,
            crate::system::SYS_INSPECT_UI_NODES_TYPE_ID,
            crate::system::SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID,
            crate::system::SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID,
            crate::system::SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID,
        ];

        for sealed_type in sealed_types {
            let error = UserStateChange::new(
                function_id(ROOT),
                String::new(),
                function_id(FUNCTION),
                String::new(),
                state_slot_id(SLOT),
                None,
                RuntimeValue::Integer(1),
                sealed_type,
            )
            .expect_err("sealed Inspector identities are transient and not USER-persistable");
            assert!(matches!(error, UserStateError::InvalidChange { .. }));
            assert_eq!(error.code(), None);
        }

        UserStateChange::new(
            function_id(ROOT),
            String::new(),
            function_id(FUNCTION),
            String::new(),
            state_slot_id(SLOT),
            None,
            RuntimeValue::Integer(1),
            type_id(TYPE_INT),
        )
        .expect("ordinary scalar USER state remains persistable");
    }

    #[test]
    fn change_rejects_a_sealed_inspector_runtime_value_with_ordinary_metadata() {
        let error = UserStateChange::new(
            function_id(ROOT),
            String::new(),
            function_id(FUNCTION),
            String::new(),
            state_slot_id(SLOT),
            None,
            RuntimeValue::Reference {
                target: crate::system::SYS_INSPECT_INVOCATION_TYPE_ID,
                object: crate::ObjectId::from_bytes([0x91; 16]),
            },
            type_id(TYPE_INT),
        )
        .expect_err("sealed Inspector runtime identities cannot hide behind ordinary metadata");
        assert!(matches!(error, UserStateError::InvalidChange { .. }));
    }


    #[test]
    fn change_rejects_a_sealed_inspector_reference_nested_in_an_invoke_request_argument() {
        let argument = InvocationArgument::new(
            InvocationParameterSelector::name("payload").expect("a valid parameter selector"),
            InvokeValue::new(RuntimeValue::Reference {
                target: crate::system::SYS_INSPECT_INVOCATION_TYPE_ID,
                object: crate::ObjectId::from_bytes([0x92; 16]),
            })
            .expect("a valid invocation argument value"),
        );
        let request = InvokeRequest::new(InvokeRequestInput {
            target: InvocationTarget::function_id(function_id(FUNCTION)),
            arguments: vec![argument],
            caller_context: InvocationCallerContext::new(
                InvocationCallerKind::CliPipe,
                false,
                false,
                None,
                None,
                "en-GB",
                "Europe/London",
                None,
            )
            .expect("a valid caller context"),
            client_offer: InvocationClientOffer::new(
                5,
                "en-GB",
                "Europe/London",
                [],
                [],
                1_024,
                0,
                None,
                None,
            )
            .expect("a valid client offer"),
            output_requirement: None,
            state_profile: None,
            trace_policy: InvocationTracePolicy::Off,
            idempotency_key: None,
            parent_invocation_id: None,
            observer_context: None,
        })
        .expect("a valid invocation request");

        let error = UserStateChange::new(
            function_id(ROOT),
            String::new(),
            function_id(FUNCTION),
            String::new(),
            state_slot_id(SLOT),
            None,
            RuntimeValue::InvokeRequest(request),
            type_id(TYPE_INT),
        )
        .expect_err("sealed Inspector values cannot hide in request arguments");
        assert!(matches!(error, UserStateError::InvalidChange { .. }));
    }

    #[test]
    fn change_rejects_a_sealed_inspector_descriptor_nested_in_a_sink_offer() {
        let descriptor = TypeDescriptor::option(
            TypeDescriptor::list(TypeDescriptor::named(
                crate::system::SYS_INSPECT_INVOCATION_TYPE_ID,
            ))
            .expect("a valid list descriptor"),
        )
        .expect("a valid option descriptor");
        let sink_offer = InvocationSinkOffer::new(
            descriptor,
            ["application/octet-stream"],
            false,
            0,
            None,
        )
        .expect("a valid sink offer");

        assert_rejected_sealed_offer_value(
            invoke_request_with_offers(vec![sink_offer], vec![]),
            "sealed Inspector descriptors cannot hide in sink offers",
        );
    }

    #[test]
    fn change_rejects_a_sealed_inspector_descriptor_nested_in_a_runtime_offer() {
        let descriptor = TypeDescriptor::map(
            TypeDescriptor::named(type_id(TYPE_INT)),
            TypeDescriptor::option(TypeDescriptor::reference(
                crate::system::SYS_INSPECT_SNAPSHOT_TYPE_ID,
            ))
            .expect("a valid option descriptor"),
        )
        .expect("a valid map descriptor");
        let runtime_offer = InvocationRuntimeOffer::new(
            "runtime",
            "1",
            [descriptor],
            [],
            0,
            false,
            None,
        )
        .expect("a valid runtime offer");

        assert_rejected_sealed_offer_value(
            invoke_request_with_offers(vec![], vec![runtime_offer]),
            "sealed Inspector descriptors cannot hide in runtime offers",
        );
    }

    #[test]
    fn change_accepts_ordinary_descriptors_in_sink_and_runtime_offers() {
        let sink_descriptor = TypeDescriptor::list(
            TypeDescriptor::option(TypeDescriptor::named(type_id(TYPE_INT)))
                .expect("a valid option descriptor"),
        )
        .expect("a valid list descriptor");
        let sink_offer = InvocationSinkOffer::new(
            sink_descriptor,
            ["application/octet-stream"],
            false,
            0,
            None,
        )
        .expect("a valid sink offer");
        let runtime_descriptor = TypeDescriptor::map(
            TypeDescriptor::named(type_id(TYPE_TEXT)),
            TypeDescriptor::option(TypeDescriptor::reference(type_id(TYPE_INT)))
                .expect("a valid option descriptor"),
        )
        .expect("a valid map descriptor");
        let runtime_offer = InvocationRuntimeOffer::new(
            "runtime",
            "1",
            [runtime_descriptor],
            [],
            0,
            false,
            None,
        )
        .expect("a valid runtime offer");

        UserStateChange::new(
            function_id(ROOT),
            String::new(),
            function_id(FUNCTION),
            String::new(),
            state_slot_id(SLOT),
            None,
            invoke_request_with_offers(vec![sink_offer], vec![runtime_offer]),
            type_id(TYPE_INT),
        )
        .expect("ordinary invocation descriptors remain USER-persistable");
    }

    #[test]
    fn change_rejects_a_sealed_inspector_reference_nested_in_an_invoke_event_value_batch() {
        let body = InvocationEventBody::value_batch(
            None,
            [
                InvokeValue::new(RuntimeValue::Reference {
                    target: crate::system::SYS_INSPECT_INVOCATION_TYPE_ID,
                    object: crate::ObjectId::from_bytes([0x93; 16]),
                })
                .expect("a valid event value"),
            ],
        )
        .expect("a non-empty value batch");
        let event = InvokeEvent::new(crate::InvocationId::from_bytes([0x94; 16]), 0, body)
            .expect("a valid invocation event");

        let error = UserStateChange::new(
            function_id(ROOT),
            String::new(),
            function_id(FUNCTION),
            String::new(),
            state_slot_id(SLOT),
            None,
            RuntimeValue::InvokeEvent(event),
            type_id(TYPE_INT),
        )
        .expect_err("sealed Inspector values cannot hide in event value batches");
        assert!(matches!(error, UserStateError::InvalidChange { .. }));
    }

    #[test]
    fn forged_sealed_inspector_cell_fails_closed_before_write() {
        let forged = cell(
            PRINCIPAL_A,
            ROOT,
            "",
            FUNCTION,
            "",
            SLOT,
            crate::system::SYS_INSPECT_SNAPSHOT_TYPE_ID,
            1,
        );
        let error = apply_change(
            Some(&forged),
            &default_change(Some(1), RuntimeValue::Integer(2)),
            principal_id(PRINCIPAL_A),
        )
        .expect_err("a forged persisted Inspector carrier identity must fail closed");
        assert!(matches!(error, UserStateError::InvalidChange { .. }));
        assert_eq!(error.code(), None);
    }

    #[test]
    fn every_key_component_distinguishes_cells() {
        let base = default_key();
        assert_ne!(base, key(PRINCIPAL_B, ROOT, "", FUNCTION, "", SLOT));
        assert_ne!(base, key(PRINCIPAL_A, 0x99, "", FUNCTION, "", SLOT));
        assert_ne!(base, key(PRINCIPAL_A, ROOT, "dark", FUNCTION, "", SLOT));
        assert_ne!(base, key(PRINCIPAL_A, ROOT, "", 0x99, "", SLOT));
        assert_ne!(base, key(PRINCIPAL_A, ROOT, "", FUNCTION, "tab-2", SLOT));
        assert_ne!(base, key(PRINCIPAL_A, ROOT, "", FUNCTION, "", 0x99));
    }

    #[test]
    fn without_principal_and_with_principal_round_trip() {
        let full = default_key();
        let partial = full.without_principal();
        assert_eq!(partial.root_function(), function_id(ROOT));
        assert_eq!(partial.state_profile(), "");
        assert_eq!(partial.function(), function_id(FUNCTION));
        assert_eq!(partial.instance_key(), "");
        assert_eq!(partial.state_slot(), state_slot_id(SLOT));
        assert_eq!(
            partial.clone().with_principal(principal_id(PRINCIPAL_A)),
            full
        );
    }

    #[test]
    fn first_write_creates_revision_one() {
        let change = default_change(None, RuntimeValue::Integer(1));
        let result = apply_change(None, &change, principal_id(PRINCIPAL_A))
            .expect("a create-only first write must succeed");
        assert_eq!(
            result.outcome(),
            UserStateWriteOutcome::Written { revision: 1 }
        );
        assert_eq!(result.key(), &key_without(ROOT, "", FUNCTION, "", SLOT));
    }

    #[test]
    fn first_write_accepts_an_explicit_revision_zero_expectation() {
        let change = default_change(Some(0), RuntimeValue::Integer(1));
        let result = apply_change(None, &change, principal_id(PRINCIPAL_A))
            .expect("expecting the absent cell's revision 0 must create it");
        assert_eq!(
            result.outcome(),
            UserStateWriteOutcome::Written { revision: 1 }
        );
    }

    #[test]
    fn first_write_with_a_nonzero_expectation_conflicts() {
        let change = default_change(Some(1), RuntimeValue::Integer(1));
        let error = apply_change(None, &change, principal_id(PRINCIPAL_A))
            .expect_err("no cell exists, so revision 1 is not the current revision");
        assert_eq!(
            error,
            UserStateError::RevisionConflict {
                key: Box::new(key_without(ROOT, "", FUNCTION, "", SLOT)),
                expected: Some(1),
                current: 0,
            }
        );
        assert_eq!(error.code(), Some("ORNA0902"));
    }

    #[test]
    fn matching_write_increments_the_revision() {
        let first = default_change(Some(3), RuntimeValue::Integer(1));
        let result = apply_change(Some(&default_cell(3)), &first, principal_id(PRINCIPAL_A))
            .expect("a matching revision must write");
        assert_eq!(
            result.outcome(),
            UserStateWriteOutcome::Written { revision: 4 }
        );

        let second = default_change(Some(4), RuntimeValue::Integer(2));
        let result = apply_change(Some(&default_cell(4)), &second, principal_id(PRINCIPAL_A))
            .expect("a second matching revision must write");
        assert_eq!(
            result.outcome(),
            UserStateWriteOutcome::Written { revision: 5 }
        );
    }

    #[test]
    fn stale_write_conflicts_with_the_current_revision() {
        let change = default_change(Some(4), RuntimeValue::Integer(1));
        let error = apply_change(Some(&default_cell(5)), &change, principal_id(PRINCIPAL_A))
            .expect_err("a stale expected revision must conflict");
        assert_eq!(
            error,
            UserStateError::RevisionConflict {
                key: Box::new(key_without(ROOT, "", FUNCTION, "", SLOT)),
                expected: Some(4),
                current: 5,
            }
        );
        assert_eq!(error.code(), Some("ORNA0902"));
    }

    #[test]
    fn future_write_conflicts_with_the_current_revision() {
        let change = default_change(Some(6), RuntimeValue::Integer(1));
        let error = apply_change(Some(&default_cell(5)), &change, principal_id(PRINCIPAL_A))
            .expect_err("an expected revision ahead of the current one must conflict");
        assert_eq!(error.code(), Some("ORNA0902"));
        assert!(matches!(
            error,
            UserStateError::RevisionConflict { current: 5, .. }
        ));
    }

    #[test]
    fn create_only_expectation_conflicts_on_an_existing_cell() {
        let change = default_change(None, RuntimeValue::Integer(1));
        let error = apply_change(Some(&default_cell(5)), &change, principal_id(PRINCIPAL_A))
            .expect_err("None requires the cell not to exist");
        assert_eq!(
            error,
            UserStateError::RevisionConflict {
                key: Box::new(key_without(ROOT, "", FUNCTION, "", SLOT)),
                expected: None,
                current: 5,
            }
        );
        assert_eq!(error.code(), Some("ORNA0902"));
    }

    #[test]
    fn revision_overflow_fails_closed() {
        let change = default_change(Some(u64::MAX), RuntimeValue::Integer(1));
        let error = apply_change(
            Some(&default_cell(u64::MAX)),
            &change,
            principal_id(PRINCIPAL_A),
        )
        .expect_err("a revision counter cannot increment past u64::MAX");
        assert!(matches!(error, UserStateError::InvalidChange { .. }));
        assert_eq!(error.code(), None);
    }

    #[test]
    fn type_mismatch_fails_closed_with_orna0901() {
        let change = change(
            ROOT,
            "",
            FUNCTION,
            "",
            SLOT,
            Some(5),
            RuntimeValue::Text("stale".to_owned()),
            type_id(TYPE_TEXT),
        );
        let error = apply_change(Some(&default_cell(5)), &change, principal_id(PRINCIPAL_A))
            .expect_err("a change value type must equal the cell value type");
        assert_eq!(
            error,
            UserStateError::TypeIncompatible {
                key: Box::new(key_without(ROOT, "", FUNCTION, "", SLOT)),
                expected: type_id(TYPE_TEXT),
                current: type_id(TYPE_INT),
            }
        );
        assert_eq!(error.code(), Some("ORNA0901"));
    }

    #[test]
    fn type_check_precedes_the_revision_check() {
        let change = change(
            ROOT,
            "",
            FUNCTION,
            "",
            SLOT,
            Some(4),
            RuntimeValue::Text("stale".to_owned()),
            type_id(TYPE_TEXT),
        );
        let error = apply_change(Some(&default_cell(5)), &change, principal_id(PRINCIPAL_A))
            .expect_err("an incompatible type fails closed before revision arithmetic");
        assert!(matches!(error, UserStateError::TypeIncompatible { .. }));
        assert_eq!(error.code(), Some("ORNA0901"));
    }

    #[test]
    fn a_cell_of_another_principal_is_rejected_with_orna0903() {
        let change = default_change(Some(5), RuntimeValue::Integer(1));
        let error = apply_change(Some(&default_cell(5)), &change, principal_id(PRINCIPAL_B))
            .expect_err("a cell of another principal must fail closed");
        assert_eq!(
            error,
            UserStateError::PrincipalSpoofAttempt {
                cell_principal: principal_id(PRINCIPAL_A),
                session_principal: principal_id(PRINCIPAL_B),
            }
        );
        assert_eq!(error.code(), Some("ORNA0903"));
        assert!(error.to_string().contains("ORNA0903"));
    }

    #[test]
    fn a_cell_of_the_session_principal_writes() {
        let change = default_change(Some(5), RuntimeValue::Integer(1));
        let result = apply_change(Some(&default_cell(5)), &change, principal_id(PRINCIPAL_A))
            .expect("the session principal's own cell must write");
        assert_eq!(
            result.outcome(),
            UserStateWriteOutcome::Written { revision: 6 }
        );
    }

    #[test]
    fn a_cell_for_a_different_key_fails_closed_as_an_invalid_change() {
        let change = change(
            ROOT,
            "",
            FUNCTION,
            "tab-2",
            SLOT,
            Some(5),
            RuntimeValue::Integer(1),
            type_id(TYPE_INT),
        );
        let error = apply_change(Some(&default_cell(5)), &change, principal_id(PRINCIPAL_A))
            .expect_err("the supplied cell must be the cell for the change key");
        assert!(matches!(error, UserStateError::InvalidChange { .. }));
        assert_eq!(error.code(), None);
    }

    #[test]
    fn write_results_carry_the_change_key_and_outcomes_in_change_order() {
        let first = default_change(None, RuntimeValue::Integer(1));
        let second = change(
            ROOT,
            "dark",
            FUNCTION,
            "tab-2",
            SLOT,
            Some(0),
            RuntimeValue::Integer(2),
            type_id(TYPE_INT),
        );

        let first_result = apply_change(None, &first, principal_id(PRINCIPAL_A))
            .expect("the first create-only write must succeed");
        let second_result = apply_change(None, &second, principal_id(PRINCIPAL_A))
            .expect("the second create-only write must succeed");

        assert_eq!(first_result.key(), &first.key_without_principal());
        assert_eq!(
            first_result.outcome(),
            UserStateWriteOutcome::Written { revision: 1 }
        );
        assert_eq!(second_result.key(), &second.key_without_principal());
        assert_eq!(second_result.key().state_profile(), "dark");
        assert_eq!(second_result.key().instance_key(), "tab-2");
        assert_eq!(
            second_result.outcome(),
            UserStateWriteOutcome::Written { revision: 1 }
        );
    }

    #[test]
    fn write_result_can_report_a_conflict_outcome() {
        let result = UserStateWriteResult::new(
            key_without(ROOT, "", FUNCTION, "", SLOT),
            UserStateWriteOutcome::Conflict {
                current_revision: 5,
            },
        );
        assert_eq!(
            result.outcome(),
            UserStateWriteOutcome::Conflict {
                current_revision: 5
            }
        );
    }

    #[test]
    fn cell_type_matches_reflects_the_declared_slot_type() {
        let cell = default_cell(3);
        assert!(cell_type_matches(&cell, type_id(TYPE_INT)));
        assert!(!cell_type_matches(&cell, type_id(TYPE_TEXT)));
    }

    #[test]
    fn cell_accessors_expose_the_durable_facts() {
        let cell = default_cell(3);
        assert_eq!(cell.key(), &default_key());
        assert_eq!(cell.value(), &RuntimeValue::Integer(7));
        assert_eq!(cell.value_type(), type_id(TYPE_INT));
        assert_eq!(cell.revision(), 3);
        assert_eq!(cell.updated_at(), SystemTime::UNIX_EPOCH);
    }

    #[test]
    fn every_error_variant_displays_and_maps_its_spec_code() {
        let key = key_without(ROOT, "", FUNCTION, "", SLOT);
        let errors = [
            UserStateError::TypeIncompatible {
                key: Box::new(key.clone()),
                expected: type_id(TYPE_TEXT),
                current: type_id(TYPE_INT),
            },
            UserStateError::RevisionConflict {
                key: Box::new(key.clone()),
                expected: Some(3),
                current: 5,
            },
            UserStateError::PrincipalSpoofAttempt {
                cell_principal: principal_id(PRINCIPAL_A),
                session_principal: principal_id(PRINCIPAL_B),
            },
            UserStateError::InvalidKey {
                reason: "a NUL byte".to_owned(),
            },
            UserStateError::InvalidChange {
                reason: "revision overflow".to_owned(),
            },
        ];
        let codes = [
            Some("ORNA0901"),
            Some("ORNA0902"),
            Some("ORNA0903"),
            None,
            None,
        ];
        for (error, code) in errors.iter().zip(codes) {
            assert_eq!(error.code(), code);
            let message = error.to_string();
            assert!(!message.is_empty());
            let _: &dyn Error = error;
        }
    }
}
