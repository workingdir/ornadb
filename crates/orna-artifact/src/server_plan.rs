//! Canonical `orna.server-plan` artifact formats.
//!
//! The version-1 byte order is:
//!
//! ```text
//! magic[8] = ORNASP\\0\\0
//! version: u32 big-endian = 1
//! input_count: u32 big-endian = 1
//! scan.input: u32 big-endian = 0
//! scan.object_type: [u8; 16]
//! projections: count:u32, expression*
//! selection: present:u8, expression?
//! ordering: count:u32, (expression, direction:u8, null_order:u8)*
//! ```
//!
//! An expression begins with its kind tag, followed by its resolved type and
//! nullability byte, then its kind payload. Identifiers are their raw opaque
//! 16-byte Orna representations. This format contains no source names, source
//! spans, PostgreSQL names, or Rust serialisation data.
//!
//! Version 2 uses the same envelope, scan, and projection encoding. It fixes
//! selection to `REF(input 0) = selector_parameter`, adds the private selector
//! parameter expression tag 5 only in that position, and requires zero
//! ordering terms.
//!
//! Version 3 uses the version-1 layout with a mandatory zero ordering count.
//! The version itself marks `SELECT DISTINCT`; it adds no expression tag or
//! set-quantifier field.

use std::fmt;

use orna_core::{
    FieldId, FunctionId, ParameterId, TypeId,
    types::{ResolvedType, StandardScalar},
};

use crate::artifact_codec::{DecodeError, Reader, Writer};
mod codec;
#[cfg(test)]
use codec::{
    decode_resolved_type, encode_count, encode_identity_selection, encode_optional_selection,
    encode_plan_prefix, encode_resolved_type,
};

/// The stable public identity of this artifact format.
pub const FORMAT_IDENTITY: &str = "orna.server-plan";
/// The Orna language version whose semantics this artifact version executes.
pub const LANGUAGE_VERSION_IDENTITY: &str = "orna.language/1";
/// The version used by no-argument SERVER query artifacts.
pub const FORMAT_VERSION: u32 = 1;
/// The version used by identity-selected SERVER query artifacts.
pub const IDENTITY_SELECTED_FORMAT_VERSION: u32 = 2;
/// The version used by parameter-free `SELECT DISTINCT` SERVER query artifacts.
pub const DISTINCT_FORMAT_VERSION: u32 = 3;
/// The version used by unique-Text-selected SERVER query artifacts.
pub const UNIQUE_TEXT_SELECTED_FORMAT_VERSION: u32 = 4;
/// The exact first eight bytes of every server-plan artifact.
pub const MAGIC: [u8; 8] = *b"ORNASP\0\0";

/// The maximum number of projections in one server plan.
pub const MAX_PROJECTIONS: u32 = 1_024;
/// The maximum number of ordering terms in one server plan.
pub const MAX_ORDERING: u32 = 1_024;
/// The maximum number of stable steps in a field path.
pub const MAX_FIELD_PATH_STEPS: u32 = 64;
/// The maximum nesting depth for expression trees.
pub const MAX_EXPRESSION_DEPTH: u32 = 64;
/// The maximum number of expression nodes across one complete plan.
pub const MAX_EXPRESSION_NODES: u32 = 8_192;
/// The maximum accepted encoded artifact size.
pub const MAX_ARTIFACT_BYTES: usize = 16 * 1024 * 1024;

const PRIMARY_INPUT: u32 = 0;
const EXACT_INPUT_COUNT: u32 = 1;
const IDENTITY_SELECTION_EXPRESSION_NODES: u32 = 3;
const UNIQUE_TEXT_SELECTION_EXPRESSION_NODES: u32 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LegacyResolvedType {
    Scalar(StandardScalar),
    Named(TypeId),
    Reference(TypeId),
}

const fn project_legacy_resolved_type(resolved_type: ResolvedType) -> Option<LegacyResolvedType> {
    match (
        resolved_type.legacy_scalar(),
        resolved_type.named_type(),
        resolved_type.value_type(),
        resolved_type.reference_target(),
    ) {
        (Some(scalar), None, None, None) => Some(LegacyResolvedType::Scalar(scalar)),
        (None, Some(type_id), None, None) => Some(LegacyResolvedType::Named(type_id)),
        (None, None, None, Some(target)) => Some(LegacyResolvedType::Reference(target)),
        _ => None,
    }
}

/// A backend-neutral checked SERVER query plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerPlan {
    /// The only object scan in version 1.
    pub scan: Scan,
    /// Expressions returned by the query.
    pub projections: Vec<Expression>,
    /// The optional `WHERE` expression.
    pub selection: Option<Expression>,
    /// Expressions used to order the returned rows.
    pub ordering: Vec<Ordering>,
}

/// A checked SERVER query plan with one fixed identity selector.
///
/// This separate model prevents the private selector parameter from appearing
/// in projections or ordering expressions. Its selection is always
/// `REF(input 0) = selector_parameter` and it has no ordering terms.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdentitySelectedServerPlan {
    scan: Scan,
    projections: Vec<Expression>,
    selector: IdentitySelector,
}

/// A checked parameter-free `SELECT DISTINCT` SERVER query plan.
///
/// This separate model makes duplicate elimination part of the artifact
/// version. It permits only the version-1 scan, projections, and optional
/// selection shape, and it cannot contain ordering or parameter expressions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DistinctServerPlan {
    scan: Scan,
    projections: Vec<Expression>,
    selection: Option<Expression>,
}

/// A checked SERVER query plan selected by one direct unique Text field.
///
/// This sealed version-four model has one fixed `SelectBindValue::Text`
/// selection and no ordering terms. It cannot represent a general parameter
/// expression or another selector form.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UniqueTextSelectedServerPlan {
    scan: Scan,
    projections: Vec<Expression>,
    selector: SelectBindValue,
}

/// The only parameter bind accepted by a version-4 server plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectBindValue {
    /// Binds one required non-null Text function parameter to one direct field.
    Text {
        /// The type scanned by the plan.
        scan_object_type: TypeId,
        /// The object type that owns the direct selected field.
        field_owner: TypeId,
        /// The direct selected field.
        field: FieldId,
        /// The function that owns the selector parameter.
        parameter_owner: FunctionId,
        /// The selector parameter.
        parameter: ParameterId,
        /// The exact resolved Text authority of the selected field and parameter.
        resolved_type: ResolvedType,
        /// Whether the selected unique Text field can contain null.
        field_nullable: bool,
        /// Whether the selector parameter is required and non-null.
        parameter_required_non_null: bool,
    },
}

/// An owner-qualified parameter used by an identity-selected plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IdentitySelector {
    owner: FunctionId,
    parameter: ParameterId,
}

impl IdentitySelector {
    /// Creates an owner-qualified selector parameter.
    pub const fn new(owner: FunctionId, parameter: ParameterId) -> Self {
        Self { owner, parameter }
    }

    /// Returns the function that owns the selector parameter.
    pub const fn owner(self) -> FunctionId {
        self.owner
    }

    /// Returns the selector parameter identity.
    pub const fn parameter(self) -> ParameterId {
        self.parameter
    }
}

/// The single source object scanned by a server plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Scan {
    /// The explicit input slot. Server plans accept only slot zero.
    pub input: u32,
    /// The stable identity of the scanned object type.
    pub object_type: TypeId,
}

/// One ordering expression and its selected ordering rules.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Ordering {
    /// The expression that supplies ordering values.
    pub expression: Expression,
    /// The source-selected ordering direction.
    pub direction: SortDirection,
    /// The source-selected null ordering rule.
    pub null_order: NullOrder,
}

/// The source-selected ordering direction, before backend default resolution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SortDirection {
    /// Use the language default ordering direction.
    Unspecified,
    /// Sort from lower to higher values.
    Ascending,
    /// Sort from higher to lower values.
    Descending,
}

/// The source-selected null ordering rule, before backend default resolution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NullOrder {
    /// Use the language default null ordering.
    Unspecified,
}

/// One checked expression with its resolved type and nullability.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Expression {
    /// The resolved meaning of this expression.
    pub kind: ExpressionKind,
    /// The exact resolved type and nullability of this expression.
    pub value_type: ValueType,
}

/// The resolved type and nullability of one expression.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ValueType {
    /// The stable resolved type.
    pub resolved_type: ResolvedType,
    /// Whether this expression can evaluate to `NULL`.
    pub nullable: bool,
}

/// The resolved meaning of one expression.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExpressionKind {
    /// A reference to the current object in an input slot.
    ObjectReference {
        /// The input slot. Version 1 accepts only slot zero.
        input: u32,
    },
    /// A stable field traversal from an input slot.
    FieldPath {
        /// The input slot. Version 1 accepts only slot zero.
        input: u32,
        /// Stable owner-field steps in traversal order.
        steps: Vec<FieldStep>,
    },
    /// A literal BOOLEAN value.
    BooleanLiteral {
        /// The literal value.
        value: bool,
    },
    /// Equality between two compatible expressions.
    Equality {
        /// The left operand.
        left: Box<Expression>,
        /// The right operand.
        right: Box<Expression>,
    },
}

/// One stable field reference in an ordered field path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FieldStep {
    /// The stable identity of the object type that owns this field.
    pub owner: TypeId,
    /// The stable identity of the field.
    pub field: FieldId,
}

/// An error returned when an artifact cannot be decoded or encoded safely.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServerPlanError {
    /// The artifact does not start with the server-plan magic bytes.
    InvalidMagic,
    /// The artifact version is not supported.
    UnsupportedVersion(u32),
    /// The artifact did not declare exactly one input.
    UnexpectedInputCount(u32),
    /// An input slot is invalid for the single-scan model.
    InvalidInputSlot(u32),
    /// An enum tag is not defined by this format version.
    InvalidEnumTag {
        /// The encoded enum category.
        kind: &'static str,
        /// The encoded tag.
        tag: u8,
    },
    /// A boolean byte was not zero or one.
    InvalidBoolean {
        /// The encoded boolean category.
        context: &'static str,
        /// The encoded byte.
        value: u8,
    },
    /// A length-prefixed collection exceeds the server-plan limit.
    CollectionLimit {
        /// The collection category.
        kind: &'static str,
        /// The encoded count.
        count: u32,
        /// The largest valid count.
        maximum: u32,
    },
    /// The encoded artifact exceeds the server-plan byte limit.
    ArtifactSizeLimit {
        /// The supplied artifact size.
        size: usize,
        /// The largest accepted artifact size.
        maximum: usize,
    },
    /// A field path contains no field steps.
    EmptyFieldPath,
    /// An expression tree exceeds the server-plan nesting limit.
    RecursionLimitExceeded,
    /// A complete plan contains too many expression nodes.
    ExpressionNodeLimitExceeded,
    /// A version-3 projection uses a type outside the accepted DISTINCT domain.
    UnsupportedDistinctProjectionType {
        /// The rejected resolved projection type.
        resolved_type: ResolvedType,
    },
    /// A version-3 artifact contains a nonzero ordering count.
    DistinctOrderingNotAllowed {
        /// The rejected encoded ordering count.
        count: u32,
    },
    /// The artifact ends before a complete field can be read.
    Truncated,
    /// The artifact contains bytes after a complete plan.
    TrailingBytes,
    /// The decoded or supplied plan violates a checked-plan invariant.
    InvalidModel(&'static str),
}

impl From<DecodeError> for ServerPlanError {
    fn from(error: DecodeError) -> Self {
        match error {
            DecodeError::Truncated => Self::Truncated,
            DecodeError::TrailingBytes => Self::TrailingBytes,
        }
    }
}

impl fmt::Display for ServerPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMagic => formatter.write_str("invalid orna.server-plan artifact magic"),
            Self::UnsupportedVersion(version) => {
                write!(
                    formatter,
                    "unsupported orna.server-plan artifact version {version}"
                )
            }
            Self::UnexpectedInputCount(count) => {
                write!(formatter, "server plan requires one input, found {count}")
            }
            Self::InvalidInputSlot(slot) => {
                write!(
                    formatter,
                    "server plan requires input slot zero, found {slot}"
                )
            }
            Self::InvalidEnumTag { kind, tag } => {
                write!(formatter, "invalid {kind} tag {tag}")
            }
            Self::InvalidBoolean { context, value } => {
                write!(formatter, "invalid {context} boolean byte {value}")
            }
            Self::CollectionLimit {
                kind,
                count,
                maximum,
            } => write!(
                formatter,
                "{kind} count {count} exceeds server-plan limit {maximum}"
            ),
            Self::ArtifactSizeLimit { size, maximum } => write!(
                formatter,
                "server plan artifact size {size} exceeds server-plan limit {maximum}"
            ),
            Self::EmptyFieldPath => {
                formatter.write_str("field path must contain at least one step")
            }
            Self::RecursionLimitExceeded => {
                formatter.write_str("server plan expression nesting exceeds server-plan limit")
            }
            Self::ExpressionNodeLimitExceeded => {
                formatter.write_str("server plan expression count exceeds server-plan limit")
            }
            Self::UnsupportedDistinctProjectionType { .. } => formatter.write_str(
                "SELECT DISTINCT projections support only BOOLEAN, INTEGER, BIGINT, BYTES, and REF values",
            ),
            Self::DistinctOrderingNotAllowed { .. } => formatter.write_str(
                "SELECT DISTINCT queries do not allow ORDER BY; remove the ORDER BY clause",
            ),
            Self::Truncated => formatter.write_str("truncated orna.server-plan artifact"),
            Self::TrailingBytes => {
                formatter.write_str("trailing bytes after orna.server-plan artifact")
            }
            Self::InvalidModel(reason) => write!(formatter, "invalid server plan model: {reason}"),
        }
    }
}

impl std::error::Error for ServerPlanError {}

#[cfg(test)]
#[path = "server_plan/tests.rs"]
mod tests;
