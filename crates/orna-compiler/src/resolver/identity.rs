//! Typed identities assigned while the resolver checks one source bundle.
//!
//! A provisional identity is local to one check. It is not a core identity and
//! cannot be converted into one. A later stage allocates durable core
//! identities after it accepts the checked result.

#![allow(
    dead_code,
    reason = "the resolver identity assignments are an isolated prerequisite for later checking work"
)]

use std::fmt;

use orna_core::{ExpressionId, FieldId, FunctionId, ParameterId, SchemaId, TypeId};

/// Constructs a checked identity without exposing a conversion API.
pub(crate) trait ResolverId<CoreId> {
    fn existing_id(id: CoreId) -> Self;
    fn provisional_id(id: u32) -> Self;
}

macro_rules! checked_identity {
    ($provisional:ident, $checked:ident, $core:ty, $kind:literal) => {
        /// A resolver-local identity for a declaration without a core identity.
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $provisional(u32);

        /// An identity used by a checked declaration.
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub enum $checked {
            /// A definition that already exists in the base catalogue.
            Existing($core),
            /// A definition first declared in the checked source bundle.
            Provisional($provisional),
        }

        impl fmt::Display for $checked {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                match self {
                    Self::Existing(id) => id.fmt(formatter),
                    // Provisional identities only identify resolver-local values in
                    // invariant diagnostics. Their stable spelling is
                    // `provisional:<kind>:<counter>`.
                    Self::Provisional(id) => write!(formatter, "provisional:{}:{}", $kind, id.0),
                }
            }
        }

        impl $checked {
            /// Returns the existing core identity, when this definition exists.
            pub const fn existing(self) -> Option<$core> {
                match self {
                    Self::Existing(id) => Some(id),
                    Self::Provisional(_) => None,
                }
            }

            /// Reports whether this identity is local to the current check.
            pub const fn is_provisional(self) -> bool {
                matches!(self, Self::Provisional(_))
            }
        }

        impl ResolverId<$core> for $checked {
            fn existing_id(id: $core) -> Self {
                Self::Existing(id)
            }

            fn provisional_id(id: u32) -> Self {
                Self::Provisional($provisional(id))
            }
        }
    };
}

checked_identity!(ProvisionalSchemaId, CheckedSchemaId, SchemaId, "schema");
checked_identity!(ProvisionalTypeId, CheckedTypeId, TypeId, "type");
checked_identity!(ProvisionalFieldId, CheckedFieldId, FieldId, "field");
checked_identity!(
    ProvisionalExpressionId,
    CheckedExpressionId,
    ExpressionId,
    "expression"
);
checked_identity!(
    ProvisionalFunctionId,
    CheckedFunctionId,
    FunctionId,
    "function"
);
checked_identity!(
    ProvisionalParameterId,
    CheckedParameterId,
    ParameterId,
    "parameter"
);

/// Assigns a typed identity to each definition that the resolver encounters.
///
/// The associated types keep identities of different definition kinds separate
/// in code that checks source declarations.
pub(crate) trait IdentityAssignments {
    type SchemaId: ResolverId<SchemaId>;
    type TypeId: ResolverId<TypeId>;
    type FieldId: ResolverId<FieldId>;
    type ExpressionId: ResolverId<ExpressionId>;
    type FunctionId: ResolverId<FunctionId>;
    type ParameterId: ResolverId<ParameterId>;

    fn schema_id(&mut self, existing: Option<SchemaId>) -> Self::SchemaId;
    fn type_id(&mut self, existing: Option<TypeId>) -> Self::TypeId;
    fn field_id(&mut self, existing: Option<FieldId>) -> Self::FieldId;
    fn expression_id(&mut self, existing: Option<ExpressionId>) -> Self::ExpressionId;
    fn function_id(&mut self, existing: Option<FunctionId>) -> Self::FunctionId;
    fn parameter_id(&mut self, existing: Option<ParameterId>) -> Self::ParameterId;
}

/// Assigns deterministic, resolver-local identities while checking source.
///
/// Each counter starts at zero. Each definition kind has its own counter, so
/// source encounter order for one kind cannot affect another kind.
pub(crate) struct CheckAssignments {
    next_schema: u32,
    next_type: u32,
    next_field: u32,
    next_expression: u32,
    next_function: u32,
    next_parameter: u32,
}

impl CheckAssignments {
    #[allow(dead_code)]
    pub(crate) const fn new() -> Self {
        Self {
            next_schema: 0,
            next_type: 0,
            next_field: 0,
            next_expression: 0,
            next_function: 0,
            next_parameter: 0,
        }
    }
}

impl IdentityAssignments for CheckAssignments {
    type SchemaId = CheckedSchemaId;
    type TypeId = CheckedTypeId;
    type FieldId = CheckedFieldId;
    type ExpressionId = CheckedExpressionId;
    type FunctionId = CheckedFunctionId;
    type ParameterId = CheckedParameterId;

    fn schema_id(&mut self, existing: Option<SchemaId>) -> Self::SchemaId {
        checked_id(existing, &mut self.next_schema)
    }

    fn type_id(&mut self, existing: Option<TypeId>) -> Self::TypeId {
        checked_id(existing, &mut self.next_type)
    }

    fn field_id(&mut self, existing: Option<FieldId>) -> Self::FieldId {
        checked_id(existing, &mut self.next_field)
    }

    fn expression_id(&mut self, existing: Option<ExpressionId>) -> Self::ExpressionId {
        checked_id(existing, &mut self.next_expression)
    }

    fn function_id(&mut self, existing: Option<FunctionId>) -> Self::FunctionId {
        checked_id(existing, &mut self.next_function)
    }

    fn parameter_id(&mut self, existing: Option<ParameterId>) -> Self::ParameterId {
        checked_id(existing, &mut self.next_parameter)
    }
}

fn checked_id<CoreId, CheckedId: ResolverId<CoreId>>(
    existing: Option<CoreId>,
    counter: &mut u32,
) -> CheckedId {
    existing.map_or_else(
        || CheckedId::provisional_id(next(counter)),
        CheckedId::existing_id,
    )
}

fn next(counter: &mut u32) -> u32 {
    let current = *counter;
    *counter = counter
        .checked_add(1)
        .expect("resolver provisional identity counter exceeded u32");
    current
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provisional_ids_are_deterministic_and_independent_per_kind() {
        let mut assignments = CheckAssignments::new();

        assert_eq!(
            assignments.schema_id(None),
            CheckedSchemaId::Provisional(ProvisionalSchemaId(0))
        );
        assert_eq!(
            assignments.type_id(None),
            CheckedTypeId::Provisional(ProvisionalTypeId(0))
        );
        assert_eq!(
            assignments.field_id(None),
            CheckedFieldId::Provisional(ProvisionalFieldId(0))
        );
        assert_eq!(
            assignments.expression_id(None),
            CheckedExpressionId::Provisional(ProvisionalExpressionId(0))
        );
        assert_eq!(
            assignments.function_id(None),
            CheckedFunctionId::Provisional(ProvisionalFunctionId(0))
        );
        assert_eq!(
            assignments.parameter_id(None),
            CheckedParameterId::Provisional(ProvisionalParameterId(0))
        );
        assert_eq!(
            assignments.schema_id(None),
            CheckedSchemaId::Provisional(ProvisionalSchemaId(1))
        );
        assert_eq!(
            assignments.type_id(None),
            CheckedTypeId::Provisional(ProvisionalTypeId(1))
        );
    }

    #[test]
    fn existing_ids_are_preserved_without_allocating_provisionals() {
        let schema = SchemaId::from_bytes([1; 16]);
        let field = FieldId::from_bytes([2; 16]);
        let mut assignments = CheckAssignments::new();

        let checked_schema = assignments.schema_id(Some(schema));
        let checked_field = assignments.field_id(Some(field));

        assert_eq!(checked_schema.existing(), Some(schema));
        assert!(!checked_schema.is_provisional());
        assert_eq!(checked_field.existing(), Some(field));
        assert!(!checked_field.is_provisional());
        assert_eq!(
            assignments.schema_id(None),
            CheckedSchemaId::Provisional(ProvisionalSchemaId(0))
        );
        assert_eq!(
            assignments.field_id(None),
            CheckedFieldId::Provisional(ProvisionalFieldId(0))
        );
    }

    #[test]
    fn checked_ids_expose_identity_state_without_a_conversion() {
        let mut assignments = CheckAssignments::new();
        let provisional = assignments.function_id(None);

        assert_eq!(provisional.existing(), None);
        assert!(provisional.is_provisional());
    }

    #[test]
    fn existing_checked_ids_format_exactly_as_core_ids() {
        macro_rules! assert_existing_display {
            ($core:ident, $checked:ident, $byte:expr) => {
                let core = $core::from_bytes([$byte; 16]);
                assert_eq!($checked::Existing(core).to_string(), core.to_string());
            };
        }

        assert_existing_display!(SchemaId, CheckedSchemaId, 1);
        assert_existing_display!(TypeId, CheckedTypeId, 2);
        assert_existing_display!(FieldId, CheckedFieldId, 3);
        assert_existing_display!(ExpressionId, CheckedExpressionId, 4);
        assert_existing_display!(FunctionId, CheckedFunctionId, 5);
        assert_existing_display!(ParameterId, CheckedParameterId, 6);
    }

    #[test]
    fn provisional_checked_ids_have_stable_diagnostic_display() {
        assert_eq!(
            CheckedSchemaId::Provisional(ProvisionalSchemaId(0)).to_string(),
            "provisional:schema:0"
        );
        assert_eq!(
            CheckedTypeId::Provisional(ProvisionalTypeId(1)).to_string(),
            "provisional:type:1"
        );
        assert_eq!(
            CheckedFieldId::Provisional(ProvisionalFieldId(2)).to_string(),
            "provisional:field:2"
        );
        assert_eq!(
            CheckedExpressionId::Provisional(ProvisionalExpressionId(3)).to_string(),
            "provisional:expression:3"
        );
        assert_eq!(
            CheckedFunctionId::Provisional(ProvisionalFunctionId(4)).to_string(),
            "provisional:function:4"
        );
        assert_eq!(
            CheckedParameterId::Provisional(ProvisionalParameterId(5)).to_string(),
            "provisional:parameter:5"
        );
    }
}
