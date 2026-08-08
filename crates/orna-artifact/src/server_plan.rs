//! Canonical `orna.server-plan` artifact format, version 1.
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

use std::fmt;

use orna_core::{
    FieldId, TypeId,
    types::{ResolvedType, StandardScalar},
};

/// The stable public identity of this artifact format.
pub const FORMAT_IDENTITY: &str = "orna.server-plan";
/// The only supported server-plan artifact version.
pub const FORMAT_VERSION: u32 = 1;
/// The exact first eight bytes of every server-plan artifact.
pub const MAGIC: [u8; 8] = *b"ORNASP\0\0";

/// The maximum number of projections in one version-1 plan.
pub const MAX_PROJECTIONS: u32 = 1_024;
/// The maximum number of ordering terms in one version-1 plan.
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

impl ServerPlan {
    /// Encodes this validated plan into canonical version-1 bytes.
    pub fn encode(&self) -> Result<Vec<u8>, ServerPlanError> {
        validate_plan(self)?;

        let mut writer = Writer::new();
        writer.bytes(&MAGIC);
        writer.u32(FORMAT_VERSION);
        writer.u32(EXACT_INPUT_COUNT);
        encode_scan(&mut writer, self.scan);
        writer.count("projections", self.projections.len(), MAX_PROJECTIONS)?;
        for expression in &self.projections {
            encode_expression(&mut writer, expression, 0)?;
        }
        match &self.selection {
            Some(expression) => {
                writer.boolean("selection presence", true);
                encode_expression(&mut writer, expression, 0)?;
            }
            None => writer.boolean("selection presence", false),
        }
        writer.count("ordering", self.ordering.len(), MAX_ORDERING)?;
        for ordering in &self.ordering {
            encode_expression(&mut writer, &ordering.expression, 0)?;
            writer.u8(encode_sort_direction(ordering.direction));
            writer.u8(encode_null_order(ordering.null_order));
        }
        let bytes = writer.finish();
        validate_artifact_size(bytes.len())?;
        Ok(bytes)
    }

    /// Decodes exactly one canonical version-1 artifact.
    pub fn decode(bytes: &[u8]) -> Result<Self, ServerPlanError> {
        validate_artifact_size(bytes.len())?;
        let mut reader = Reader::new(bytes);
        if reader.array::<8>()? != MAGIC {
            return Err(ServerPlanError::InvalidMagic);
        }
        let version = reader.u32()?;
        if version != FORMAT_VERSION {
            return Err(ServerPlanError::UnsupportedVersion(version));
        }
        let input_count = reader.u32()?;
        if input_count != EXACT_INPUT_COUNT {
            return Err(ServerPlanError::UnexpectedInputCount(input_count));
        }
        let scan = decode_scan(&mut reader)?;
        let projection_count = reader.count("projections", MAX_PROJECTIONS)?;
        if projection_count == 0 {
            return Err(ServerPlanError::InvalidModel(
                "a server plan must contain at least one projection",
            ));
        }
        let mut projections = Vec::with_capacity(projection_count as usize);
        let mut remaining_expression_nodes = MAX_EXPRESSION_NODES;
        for _ in 0..projection_count {
            projections.push(decode_expression(
                &mut reader,
                0,
                &mut remaining_expression_nodes,
            )?);
        }
        let selection = match reader.boolean("selection presence")? {
            true => Some(decode_expression(
                &mut reader,
                0,
                &mut remaining_expression_nodes,
            )?),
            false => None,
        };
        let ordering_count = reader.count("ordering", MAX_ORDERING)?;
        let mut ordering = Vec::with_capacity(ordering_count as usize);
        for _ in 0..ordering_count {
            ordering.push(Ordering {
                expression: decode_expression(&mut reader, 0, &mut remaining_expression_nodes)?,
                direction: decode_sort_direction(reader.u8()?)?,
                null_order: decode_null_order(reader.u8()?)?,
            });
        }
        reader.require_finished()?;

        let plan = Self {
            scan,
            projections,
            selection,
            ordering,
        };
        validate_plan(&plan)?;
        Ok(plan)
    }
}

/// The single source object scanned by a version-1 plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Scan {
    /// The explicit input slot. Version 1 accepts only slot zero.
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServerPlanError {
    /// The artifact does not start with the version-1 magic bytes.
    InvalidMagic,
    /// The artifact version is not supported.
    UnsupportedVersion(u32),
    /// The version-1 artifact did not declare exactly one input.
    UnexpectedInputCount(u32),
    /// An input slot is invalid for the version-1 single-scan model.
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
    /// A length-prefixed collection exceeds the version-1 limit.
    CollectionLimit {
        /// The collection category.
        kind: &'static str,
        /// The encoded count.
        count: u32,
        /// The largest valid count.
        maximum: u32,
    },
    /// The encoded artifact exceeds the version-1 byte limit.
    ArtifactSizeLimit {
        /// The supplied artifact size.
        size: usize,
        /// The largest accepted artifact size.
        maximum: usize,
    },
    /// A field path contains no field steps.
    EmptyFieldPath,
    /// An expression tree exceeds the version-1 nesting limit.
    RecursionLimitExceeded,
    /// A complete plan contains too many expression nodes.
    ExpressionNodeLimitExceeded,
    /// The artifact ends before a complete field can be read.
    Truncated,
    /// The artifact contains bytes after a complete plan.
    TrailingBytes,
    /// The decoded or supplied plan violates a checked-plan invariant.
    InvalidModel(&'static str),
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
                write!(
                    formatter,
                    "version-1 server plan requires one input, found {count}"
                )
            }
            Self::InvalidInputSlot(slot) => {
                write!(
                    formatter,
                    "version-1 server plan requires input slot zero, found {slot}"
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
                "{kind} count {count} exceeds version-1 limit {maximum}"
            ),
            Self::ArtifactSizeLimit { size, maximum } => write!(
                formatter,
                "server plan artifact size {size} exceeds version-1 limit {maximum}"
            ),
            Self::EmptyFieldPath => {
                formatter.write_str("field path must contain at least one step")
            }
            Self::RecursionLimitExceeded => {
                formatter.write_str("server plan expression nesting exceeds version-1 limit")
            }
            Self::ExpressionNodeLimitExceeded => {
                formatter.write_str("server plan expression count exceeds version-1 limit")
            }
            Self::Truncated => formatter.write_str("truncated orna.server-plan artifact"),
            Self::TrailingBytes => {
                formatter.write_str("trailing bytes after orna.server-plan artifact")
            }
            Self::InvalidModel(reason) => write!(formatter, "invalid server plan model: {reason}"),
        }
    }
}

impl std::error::Error for ServerPlanError {}

fn validate_plan(plan: &ServerPlan) -> Result<(), ServerPlanError> {
    if plan.scan.input != PRIMARY_INPUT {
        return Err(ServerPlanError::InvalidInputSlot(plan.scan.input));
    }
    validate_count("projections", plan.projections.len(), MAX_PROJECTIONS)?;
    if plan.projections.is_empty() {
        return Err(ServerPlanError::InvalidModel(
            "a server plan must contain at least one projection",
        ));
    }
    let mut remaining_expression_nodes = MAX_EXPRESSION_NODES;
    for expression in &plan.projections {
        validate_expression(expression, plan.scan, 0, &mut remaining_expression_nodes)?;
    }
    if let Some(selection) = &plan.selection {
        validate_expression(selection, plan.scan, 0, &mut remaining_expression_nodes)?;
        if selection.value_type.resolved_type != ResolvedType::scalar(StandardScalar::Boolean) {
            return Err(ServerPlanError::InvalidModel(
                "a selection must have resolved BOOLEAN type",
            ));
        }
    }
    validate_count("ordering", plan.ordering.len(), MAX_ORDERING)?;
    for ordering in &plan.ordering {
        validate_expression(
            &ordering.expression,
            plan.scan,
            0,
            &mut remaining_expression_nodes,
        )?;
    }
    Ok(())
}

fn validate_expression(
    expression: &Expression,
    scan: Scan,
    depth: u32,
    remaining_nodes: &mut u32,
) -> Result<(), ServerPlanError> {
    if depth >= MAX_EXPRESSION_DEPTH {
        return Err(ServerPlanError::RecursionLimitExceeded);
    }
    if *remaining_nodes == 0 {
        return Err(ServerPlanError::ExpressionNodeLimitExceeded);
    }
    *remaining_nodes -= 1;

    match &expression.kind {
        ExpressionKind::ObjectReference { input } => {
            validate_input(*input)?;
            if expression.value_type
                != (ValueType {
                    resolved_type: ResolvedType::reference(scan.object_type),
                    nullable: false,
                })
            {
                return Err(ServerPlanError::InvalidModel(
                    "an object reference must be a non-null reference to the scan object type",
                ));
            }
        }
        ExpressionKind::FieldPath { input, steps } => {
            validate_input(*input)?;
            validate_count("field path steps", steps.len(), MAX_FIELD_PATH_STEPS)?;
            if steps.is_empty() {
                return Err(ServerPlanError::EmptyFieldPath);
            }
            if steps[0].owner != scan.object_type {
                return Err(ServerPlanError::InvalidModel(
                    "a field path must start at the scanned object type",
                ));
            }
        }
        ExpressionKind::BooleanLiteral { .. } => {
            if expression.value_type
                != (ValueType {
                    resolved_type: ResolvedType::scalar(StandardScalar::Boolean),
                    nullable: false,
                })
            {
                return Err(ServerPlanError::InvalidModel(
                    "a BOOLEAN literal must be non-null BOOLEAN",
                ));
            }
        }
        ExpressionKind::Equality { left, right } => {
            validate_expression(left, scan, depth + 1, remaining_nodes)?;
            validate_expression(right, scan, depth + 1, remaining_nodes)?;
            if left.value_type.resolved_type != right.value_type.resolved_type {
                return Err(ServerPlanError::InvalidModel(
                    "equality operands must have the same resolved type",
                ));
            }
            let expected = ValueType {
                resolved_type: ResolvedType::scalar(StandardScalar::Boolean),
                nullable: left.value_type.nullable || right.value_type.nullable,
            };
            if expression.value_type != expected {
                return Err(ServerPlanError::InvalidModel(
                    "equality must have BOOLEAN type and SQL nullability",
                ));
            }
        }
    }
    Ok(())
}

fn validate_input(input: u32) -> Result<(), ServerPlanError> {
    if input == PRIMARY_INPUT {
        Ok(())
    } else {
        Err(ServerPlanError::InvalidInputSlot(input))
    }
}

fn validate_count(kind: &'static str, count: usize, maximum: u32) -> Result<u32, ServerPlanError> {
    let count = u32::try_from(count).map_err(|_| ServerPlanError::CollectionLimit {
        kind,
        count: u32::MAX,
        maximum,
    })?;
    if count > maximum {
        return Err(ServerPlanError::CollectionLimit {
            kind,
            count,
            maximum,
        });
    }
    Ok(count)
}

fn validate_artifact_size(size: usize) -> Result<(), ServerPlanError> {
    if size > MAX_ARTIFACT_BYTES {
        Err(ServerPlanError::ArtifactSizeLimit {
            size,
            maximum: MAX_ARTIFACT_BYTES,
        })
    } else {
        Ok(())
    }
}

fn encode_scan(writer: &mut Writer, scan: Scan) {
    writer.u32(scan.input);
    writer.type_id(scan.object_type);
}

fn decode_scan(reader: &mut Reader<'_>) -> Result<Scan, ServerPlanError> {
    let input = reader.u32()?;
    validate_input(input)?;
    Ok(Scan {
        input,
        object_type: reader.type_id()?,
    })
}

fn encode_expression(
    writer: &mut Writer,
    expression: &Expression,
    depth: u32,
) -> Result<(), ServerPlanError> {
    if depth >= MAX_EXPRESSION_DEPTH {
        return Err(ServerPlanError::RecursionLimitExceeded);
    }
    match &expression.kind {
        ExpressionKind::ObjectReference { input } => {
            writer.u8(1);
            encode_value_type(writer, expression.value_type);
            writer.u32(*input);
        }
        ExpressionKind::FieldPath { input, steps } => {
            writer.u8(2);
            encode_value_type(writer, expression.value_type);
            writer.u32(*input);
            writer.count("field path steps", steps.len(), MAX_FIELD_PATH_STEPS)?;
            for step in steps {
                writer.type_id(step.owner);
                writer.field_id(step.field);
            }
        }
        ExpressionKind::BooleanLiteral { value } => {
            writer.u8(3);
            encode_value_type(writer, expression.value_type);
            writer.boolean("literal", *value);
        }
        ExpressionKind::Equality { left, right } => {
            writer.u8(4);
            encode_value_type(writer, expression.value_type);
            encode_expression(writer, left, depth + 1)?;
            encode_expression(writer, right, depth + 1)?;
        }
    }
    Ok(())
}

fn decode_expression(
    reader: &mut Reader<'_>,
    depth: u32,
    remaining_nodes: &mut u32,
) -> Result<Expression, ServerPlanError> {
    if depth >= MAX_EXPRESSION_DEPTH {
        return Err(ServerPlanError::RecursionLimitExceeded);
    }
    if *remaining_nodes == 0 {
        return Err(ServerPlanError::ExpressionNodeLimitExceeded);
    }
    *remaining_nodes -= 1;
    let tag = reader.u8()?;
    if !matches!(tag, 1..=4) {
        return Err(ServerPlanError::InvalidEnumTag {
            kind: "expression kind",
            tag,
        });
    }
    let value_type = decode_value_type(reader)?;
    let kind = match tag {
        1 => ExpressionKind::ObjectReference {
            input: reader.u32()?,
        },
        2 => {
            let input = reader.u32()?;
            let count = reader.count("field path steps", MAX_FIELD_PATH_STEPS)?;
            if count == 0 {
                return Err(ServerPlanError::EmptyFieldPath);
            }
            let mut steps = Vec::with_capacity(count as usize);
            for _ in 0..count {
                steps.push(FieldStep {
                    owner: reader.type_id()?,
                    field: reader.field_id()?,
                });
            }
            ExpressionKind::FieldPath { input, steps }
        }
        3 => ExpressionKind::BooleanLiteral {
            value: reader.boolean("literal")?,
        },
        4 => ExpressionKind::Equality {
            left: Box::new(decode_expression(reader, depth + 1, remaining_nodes)?),
            right: Box::new(decode_expression(reader, depth + 1, remaining_nodes)?),
        },
        _ => unreachable!("expression tag was validated before its payload"),
    };
    Ok(Expression { kind, value_type })
}

fn encode_value_type(writer: &mut Writer, value_type: ValueType) {
    encode_resolved_type(writer, value_type.resolved_type);
    writer.boolean("expression nullability", value_type.nullable);
}

fn decode_value_type(reader: &mut Reader<'_>) -> Result<ValueType, ServerPlanError> {
    Ok(ValueType {
        resolved_type: decode_resolved_type(reader)?,
        nullable: reader.boolean("expression nullability")?,
    })
}

fn encode_resolved_type(writer: &mut Writer, resolved_type: ResolvedType) {
    match resolved_type {
        ResolvedType::Scalar(scalar) => {
            writer.u8(1);
            writer.u8(encode_standard_scalar(scalar));
        }
        ResolvedType::Named(type_id) => {
            writer.u8(2);
            writer.type_id(type_id);
        }
        ResolvedType::Reference { target } => {
            writer.u8(3);
            writer.type_id(target);
        }
    }
}

fn decode_resolved_type(reader: &mut Reader<'_>) -> Result<ResolvedType, ServerPlanError> {
    match reader.u8()? {
        1 => Ok(ResolvedType::scalar(decode_standard_scalar(reader.u8()?)?)),
        2 => Ok(ResolvedType::named(reader.type_id()?)),
        3 => Ok(ResolvedType::reference(reader.type_id()?)),
        tag => Err(ServerPlanError::InvalidEnumTag {
            kind: "resolved type",
            tag,
        }),
    }
}

fn encode_standard_scalar(scalar: StandardScalar) -> u8 {
    match scalar {
        StandardScalar::Boolean => 1,
        StandardScalar::Integer => 2,
        StandardScalar::BigInt => 3,
        StandardScalar::Float => 4,
        StandardScalar::Decimal => 5,
        StandardScalar::CharacterLargeObject => 6,
        StandardScalar::BinaryLargeObject => 7,
        StandardScalar::Uuid => 8,
        StandardScalar::Date => 9,
        StandardScalar::Time => 10,
        StandardScalar::Timestamp => 11,
        StandardScalar::Duration => 12,
        StandardScalar::Void => 13,
    }
}

fn decode_standard_scalar(tag: u8) -> Result<StandardScalar, ServerPlanError> {
    match tag {
        1 => Ok(StandardScalar::Boolean),
        2 => Ok(StandardScalar::Integer),
        3 => Ok(StandardScalar::BigInt),
        4 => Ok(StandardScalar::Float),
        5 => Ok(StandardScalar::Decimal),
        6 => Ok(StandardScalar::CharacterLargeObject),
        7 => Ok(StandardScalar::BinaryLargeObject),
        8 => Ok(StandardScalar::Uuid),
        9 => Ok(StandardScalar::Date),
        10 => Ok(StandardScalar::Time),
        11 => Ok(StandardScalar::Timestamp),
        12 => Ok(StandardScalar::Duration),
        13 => Ok(StandardScalar::Void),
        tag => Err(ServerPlanError::InvalidEnumTag {
            kind: "standard scalar",
            tag,
        }),
    }
}

fn encode_sort_direction(direction: SortDirection) -> u8 {
    match direction {
        SortDirection::Unspecified => 1,
        SortDirection::Ascending => 2,
        SortDirection::Descending => 3,
    }
}

fn decode_sort_direction(tag: u8) -> Result<SortDirection, ServerPlanError> {
    match tag {
        1 => Ok(SortDirection::Unspecified),
        2 => Ok(SortDirection::Ascending),
        3 => Ok(SortDirection::Descending),
        tag => Err(ServerPlanError::InvalidEnumTag {
            kind: "sort direction",
            tag,
        }),
    }
}

fn encode_null_order(null_order: NullOrder) -> u8 {
    match null_order {
        NullOrder::Unspecified => 1,
    }
}

fn decode_null_order(tag: u8) -> Result<NullOrder, ServerPlanError> {
    match tag {
        1 => Ok(NullOrder::Unspecified),
        tag => Err(ServerPlanError::InvalidEnumTag {
            kind: "null order",
            tag,
        }),
    }
}

struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }

    fn bytes(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn boolean(&mut self, context: &'static str, value: bool) {
        let _ = context;
        self.u8(u8::from(value));
    }

    fn count(
        &mut self,
        kind: &'static str,
        count: usize,
        maximum: u32,
    ) -> Result<(), ServerPlanError> {
        self.u32(validate_count(kind, count, maximum)?);
        Ok(())
    }

    fn type_id(&mut self, id: TypeId) {
        self.bytes(&id.to_bytes());
    }

    fn field_id(&mut self, id: FieldId) {
        self.bytes(&id.to_bytes());
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn array<const LENGTH: usize>(&mut self) -> Result<[u8; LENGTH], ServerPlanError> {
        let bytes = self.take(LENGTH)?;
        bytes.try_into().map_err(|_| ServerPlanError::Truncated)
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], ServerPlanError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(ServerPlanError::Truncated)?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or(ServerPlanError::Truncated)?;
        self.offset = end;
        Ok(bytes)
    }

    fn u8(&mut self) -> Result<u8, ServerPlanError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, ServerPlanError> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    fn boolean(&mut self, context: &'static str) -> Result<bool, ServerPlanError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            value => Err(ServerPlanError::InvalidBoolean { context, value }),
        }
    }

    fn count(&mut self, kind: &'static str, maximum: u32) -> Result<u32, ServerPlanError> {
        let count = self.u32()?;
        if count > maximum {
            return Err(ServerPlanError::CollectionLimit {
                kind,
                count,
                maximum,
            });
        }
        Ok(count)
    }

    fn type_id(&mut self) -> Result<TypeId, ServerPlanError> {
        Ok(TypeId::from_bytes(self.array()?))
    }

    fn field_id(&mut self) -> Result<FieldId, ServerPlanError> {
        Ok(FieldId::from_bytes(self.array()?))
    }

    fn require_finished(&self) -> Result<(), ServerPlanError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(ServerPlanError::TrailingBytes)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TASK: TypeId = TypeId::from_bytes([1; 16]);
    const PERSON: TypeId = TypeId::from_bytes([2; 16]);
    const TITLE: FieldId = FieldId::from_bytes([3; 16]);
    const ASSIGNEE: FieldId = FieldId::from_bytes([4; 16]);
    const NAME: FieldId = FieldId::from_bytes([5; 16]);

    fn value_type(resolved_type: ResolvedType, nullable: bool) -> ValueType {
        ValueType {
            resolved_type,
            nullable,
        }
    }

    fn object_reference() -> Expression {
        Expression {
            kind: ExpressionKind::ObjectReference { input: 0 },
            value_type: value_type(ResolvedType::reference(TASK), false),
        }
    }

    fn title_path() -> Expression {
        Expression {
            kind: ExpressionKind::FieldPath {
                input: 0,
                steps: vec![FieldStep {
                    owner: TASK,
                    field: TITLE,
                }],
            },
            value_type: value_type(
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                true,
            ),
        }
    }

    fn assigned_name_path() -> Expression {
        Expression {
            kind: ExpressionKind::FieldPath {
                input: 0,
                steps: vec![
                    FieldStep {
                        owner: TASK,
                        field: ASSIGNEE,
                    },
                    FieldStep {
                        owner: PERSON,
                        field: NAME,
                    },
                ],
            },
            value_type: value_type(ResolvedType::named(PERSON), true),
        }
    }

    fn boolean(value: bool) -> Expression {
        Expression {
            kind: ExpressionKind::BooleanLiteral { value },
            value_type: value_type(ResolvedType::scalar(StandardScalar::Boolean), false),
        }
    }

    fn equality(left: Expression, right: Expression) -> Expression {
        Expression {
            value_type: value_type(
                ResolvedType::scalar(StandardScalar::Boolean),
                left.value_type.nullable || right.value_type.nullable,
            ),
            kind: ExpressionKind::Equality {
                left: Box::new(left),
                right: Box::new(right),
            },
        }
    }

    fn full_boolean_tree(depth: u32) -> Expression {
        if depth == 0 {
            boolean(true)
        } else {
            equality(full_boolean_tree(depth - 1), full_boolean_tree(depth - 1))
        }
    }

    fn plan() -> ServerPlan {
        ServerPlan {
            scan: Scan {
                input: 0,
                object_type: TASK,
            },
            projections: vec![
                object_reference(),
                title_path(),
                assigned_name_path(),
                equality(boolean(true), boolean(false)),
            ],
            selection: Some(equality(boolean(true), boolean(false))),
            ordering: vec![Ordering {
                expression: title_path(),
                direction: SortDirection::Descending,
                null_order: NullOrder::Unspecified,
            }],
        }
    }

    #[test]
    fn round_trips_each_current_expression_kind_and_resolved_type_shape() {
        let plan = plan();
        let encoded = plan.encode().unwrap();

        assert_eq!(ServerPlan::decode(&encoded), Ok(plan));
        assert_eq!(ServerPlan::decode(&encoded).unwrap().encode(), Ok(encoded));
    }

    #[test]
    fn encodes_a_deterministic_header_and_bytes() {
        let minimal = ServerPlan {
            scan: Scan {
                input: 0,
                object_type: TASK,
            },
            projections: vec![boolean(true)],
            selection: None,
            ordering: Vec::new(),
        };
        let encoded = minimal.encode().unwrap();

        assert_eq!(&encoded[..8], &MAGIC);
        assert_eq!(&encoded[8..12], &FORMAT_VERSION.to_be_bytes());
        assert_eq!(&encoded[12..16], &1_u32.to_be_bytes());
        assert_eq!(
            encoded,
            vec![
                79, 82, 78, 65, 83, 80, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1,
                1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 1, 3, 1, 1, 0, 1, 0, 0, 0, 0, 0,
            ]
        );
    }

    #[test]
    fn rejects_corrupt_header_enum_boolean_and_trailing_data() {
        let valid = plan().encode().unwrap();

        let mut magic = valid.clone();
        magic[0] = b'X';
        assert_eq!(
            ServerPlan::decode(&magic),
            Err(ServerPlanError::InvalidMagic)
        );

        let mut version = valid.clone();
        version[11] = 2;
        assert_eq!(
            ServerPlan::decode(&version),
            Err(ServerPlanError::UnsupportedVersion(2))
        );

        let mut input_count = valid.clone();
        input_count[12..16].copy_from_slice(&2_u32.to_be_bytes());
        assert_eq!(
            ServerPlan::decode(&input_count),
            Err(ServerPlanError::UnexpectedInputCount(2))
        );

        let mut expression_tag = valid.clone();
        expression_tag[40] = 99;
        assert_eq!(
            ServerPlan::decode(&expression_tag),
            Err(ServerPlanError::InvalidEnumTag {
                kind: "expression kind",
                tag: 99,
            })
        );

        let mut nullable = valid.clone();
        nullable[58] = 2;
        assert_eq!(
            ServerPlan::decode(&nullable),
            Err(ServerPlanError::InvalidBoolean {
                context: "expression nullability",
                value: 2,
            })
        );

        let mut trailing = valid;
        trailing.push(0);
        assert_eq!(
            ServerPlan::decode(&trailing),
            Err(ServerPlanError::TrailingBytes)
        );
    }

    #[test]
    fn rejects_truncation_oversized_counts_and_deep_nesting() {
        let valid = plan().encode().unwrap();
        assert_eq!(
            ServerPlan::decode(&valid[..valid.len() - 1]),
            Err(ServerPlanError::Truncated)
        );

        let mut projection_count = valid.clone();
        projection_count[36..40].copy_from_slice(&(MAX_PROJECTIONS + 1).to_be_bytes());
        assert_eq!(
            ServerPlan::decode(&projection_count),
            Err(ServerPlanError::CollectionLimit {
                kind: "projections",
                count: MAX_PROJECTIONS + 1,
                maximum: MAX_PROJECTIONS,
            })
        );

        let mut expression = boolean(true);
        for _ in 0..MAX_EXPRESSION_DEPTH {
            expression = equality(expression, boolean(false));
        }
        let deep = ServerPlan {
            scan: Scan {
                input: 0,
                object_type: TASK,
            },
            projections: vec![expression],
            selection: None,
            ordering: Vec::new(),
        };
        assert_eq!(deep.encode(), Err(ServerPlanError::RecursionLimitExceeded));

        let too_many_nodes = ServerPlan {
            scan: plan().scan,
            projections: vec![full_boolean_tree(13)],
            selection: None,
            ordering: Vec::new(),
        };
        assert_eq!(
            too_many_nodes.encode(),
            Err(ServerPlanError::ExpressionNodeLimitExceeded)
        );

        let oversized_artifact = vec![0; MAX_ARTIFACT_BYTES + 1];
        assert_eq!(
            ServerPlan::decode(&oversized_artifact),
            Err(ServerPlanError::ArtifactSizeLimit {
                size: MAX_ARTIFACT_BYTES + 1,
                maximum: MAX_ARTIFACT_BYTES,
            })
        );
    }

    #[test]
    fn rejects_invalid_checked_plan_invariants() {
        let mut invalid_slot = plan();
        invalid_slot.scan.input = 1;
        assert_eq!(
            invalid_slot.encode(),
            Err(ServerPlanError::InvalidInputSlot(1))
        );

        let empty_projections = ServerPlan {
            scan: plan().scan,
            projections: Vec::new(),
            selection: None,
            ordering: Vec::new(),
        };
        assert_eq!(
            empty_projections.encode(),
            Err(ServerPlanError::InvalidModel(
                "a server plan must contain at least one projection"
            ))
        );

        let mut invalid_literal = plan();
        invalid_literal.projections[3].value_type.nullable = true;
        assert_eq!(
            invalid_literal.encode(),
            Err(ServerPlanError::InvalidModel(
                "equality must have BOOLEAN type and SQL nullability"
            ))
        );

        let empty_path = ServerPlan {
            scan: plan().scan,
            projections: vec![Expression {
                kind: ExpressionKind::FieldPath {
                    input: 0,
                    steps: Vec::new(),
                },
                value_type: value_type(ResolvedType::scalar(StandardScalar::Integer), false),
            }],
            selection: None,
            ordering: Vec::new(),
        };
        assert_eq!(empty_path.encode(), Err(ServerPlanError::EmptyFieldPath));

        let wrong_path_owner = ServerPlan {
            scan: plan().scan,
            projections: vec![Expression {
                kind: ExpressionKind::FieldPath {
                    input: 0,
                    steps: vec![FieldStep {
                        owner: PERSON,
                        field: NAME,
                    }],
                },
                value_type: value_type(ResolvedType::scalar(StandardScalar::Integer), false),
            }],
            selection: None,
            ordering: Vec::new(),
        };
        assert_eq!(
            wrong_path_owner.encode(),
            Err(ServerPlanError::InvalidModel(
                "a field path must start at the scanned object type"
            ))
        );
    }
}
