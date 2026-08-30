use super::*;
impl ServerPlan {
    /// Encodes this validated plan into canonical version-1 bytes.
    pub fn encode(&self) -> Result<Vec<u8>, ServerPlanError> {
        validate_plan(self)?;

        let mut writer = encode_plan_prefix(FORMAT_VERSION, self.scan, &self.projections)?;
        encode_optional_selection(&mut writer, self.selection.as_ref())?;
        encode_count(&mut writer, "ordering", self.ordering.len(), MAX_ORDERING)?;
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
        let (mut reader, scan, projections, mut remaining_expression_nodes) =
            decode_plan_prefix(bytes, FORMAT_VERSION)?;
        let selection = decode_optional_selection(&mut reader, &mut remaining_expression_nodes)?;
        let ordering_count = decode_count(&mut reader, "ordering", MAX_ORDERING)?;
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
impl IdentitySelectedServerPlan {
    /// Creates a checked identity-selected plan.
    pub fn new(
        scan: Scan,
        projections: impl IntoIterator<Item = Expression>,
        selector: IdentitySelector,
    ) -> Result<Self, ServerPlanError> {
        let plan = Self {
            scan,
            projections: collect_projections(projections)?,
            selector,
        };
        validate_identity_selected_plan(&plan)?;
        Ok(plan)
    }

    /// Returns the canonical version for identity-selected artifacts.
    pub const fn format_version(&self) -> u32 {
        IDENTITY_SELECTED_FORMAT_VERSION
    }

    /// Returns the single source object scan.
    pub const fn scan(&self) -> Scan {
        self.scan
    }

    /// Returns projection expressions in source order.
    pub fn projections(&self) -> &[Expression] {
        &self.projections
    }

    /// Returns the owner-qualified selector parameter.
    pub const fn selector(&self) -> IdentitySelector {
        self.selector
    }

    /// Encodes this checked plan into canonical version-2 bytes.
    pub fn encode(&self) -> Result<Vec<u8>, ServerPlanError> {
        validate_identity_selected_plan(self)?;

        let mut writer = encode_plan_prefix(
            IDENTITY_SELECTED_FORMAT_VERSION,
            self.scan,
            &self.projections,
        )?;
        writer.boolean(true);
        encode_identity_selection(&mut writer, self.scan, self.selector)?;
        encode_count(&mut writer, "ordering", 0, MAX_ORDERING)?;
        let bytes = writer.finish();
        validate_artifact_size(bytes.len())?;
        Ok(bytes)
    }

    /// Decodes exactly one canonical version-2 identity-selected artifact.
    pub fn decode(bytes: &[u8]) -> Result<Self, ServerPlanError> {
        let (mut reader, scan, projections, mut remaining_expression_nodes) =
            decode_plan_prefix(bytes, IDENTITY_SELECTED_FORMAT_VERSION)?;
        if !decode_boolean(&mut reader, "selection presence")? {
            return Err(ServerPlanError::InvalidModel(
                "an identity-selected server plan must contain its fixed selection",
            ));
        }
        consume_identity_selection_nodes(&mut remaining_expression_nodes)?;
        let selector = decode_identity_selection(&mut reader, scan)?;
        let ordering_count = decode_count(&mut reader, "ordering", MAX_ORDERING)?;
        if ordering_count != 0 {
            return Err(ServerPlanError::InvalidModel(
                "an identity-selected server plan must not contain ordering terms",
            ));
        }
        reader.require_finished()?;
        Self::new(scan, projections, selector)
    }
}
impl UniqueTextSelectedServerPlan {
    /// Creates a checked unique-Text-selected plan.
    pub fn new(
        scan: Scan,
        projections: impl IntoIterator<Item = Expression>,
        selector: SelectBindValue,
    ) -> Result<Self, ServerPlanError> {
        let plan = Self {
            scan,
            projections: collect_projections(projections)?,
            selector,
        };
        validate_unique_text_selected_plan(&plan)?;
        Ok(plan)
    }

    /// Returns the canonical version for unique-Text-selected artifacts.
    pub const fn format_version(&self) -> u32 {
        UNIQUE_TEXT_SELECTED_FORMAT_VERSION
    }

    /// Returns the single source object scan.
    pub const fn scan(&self) -> Scan {
        self.scan
    }

    /// Returns projection expressions in source order.
    pub fn projections(&self) -> &[Expression] {
        &self.projections
    }

    /// Returns the one fixed Text bind selector.
    pub const fn selector(&self) -> &SelectBindValue {
        &self.selector
    }

    /// Encodes this checked plan into canonical version-4 bytes.
    pub fn encode(&self) -> Result<Vec<u8>, ServerPlanError> {
        validate_unique_text_selected_plan(self)?;

        let mut writer = encode_plan_prefix(
            UNIQUE_TEXT_SELECTED_FORMAT_VERSION,
            self.scan,
            &self.projections,
        )?;
        writer.boolean(true);
        encode_unique_text_selection(&mut writer, self.selector)?;
        encode_count(&mut writer, "ordering", 0, MAX_ORDERING)?;
        let bytes = writer.finish();
        validate_artifact_size(bytes.len())?;
        Ok(bytes)
    }

    /// Decodes exactly one canonical version-4 unique-Text-selected artifact.
    pub fn decode(bytes: &[u8]) -> Result<Self, ServerPlanError> {
        let (mut reader, scan, projections, mut remaining_expression_nodes) =
            decode_plan_prefix(bytes, UNIQUE_TEXT_SELECTED_FORMAT_VERSION)?;
        if !decode_boolean(&mut reader, "selection presence")? {
            return Err(ServerPlanError::InvalidModel(
                "a unique-Text-selected server plan must contain its fixed selection",
            ));
        }
        consume_unique_text_selection_nodes(&mut remaining_expression_nodes)?;
        let selector = decode_unique_text_selection(&mut reader)?;
        let ordering_count = decode_count(&mut reader, "ordering", MAX_ORDERING)?;
        if ordering_count != 0 {
            return Err(ServerPlanError::InvalidModel(
                "a unique-Text-selected server plan must not contain ordering terms",
            ));
        }
        reader.require_finished()?;
        Self::new(scan, projections, selector)
    }
}
impl DistinctServerPlan {
    /// Creates a checked parameter-free `SELECT DISTINCT` plan.
    pub fn new(
        scan: Scan,
        projections: impl IntoIterator<Item = Expression>,
        selection: Option<Expression>,
    ) -> Result<Self, ServerPlanError> {
        let plan = Self {
            scan,
            projections: collect_projections(projections)?,
            selection,
        };
        validate_distinct_plan(&plan)?;
        Ok(plan)
    }

    /// Returns the canonical version for `SELECT DISTINCT` artifacts.
    pub const fn format_version(&self) -> u32 {
        DISTINCT_FORMAT_VERSION
    }

    /// Returns the single source object scan.
    pub const fn scan(&self) -> Scan {
        self.scan
    }

    /// Returns projection expressions in source order.
    pub fn projections(&self) -> &[Expression] {
        &self.projections
    }

    /// Returns the optional `WHERE` expression.
    pub fn selection(&self) -> Option<&Expression> {
        self.selection.as_ref()
    }

    /// Encodes this checked plan into canonical version-3 bytes.
    pub fn encode(&self) -> Result<Vec<u8>, ServerPlanError> {
        validate_distinct_plan(self)?;

        let mut writer = encode_plan_prefix(DISTINCT_FORMAT_VERSION, self.scan, &self.projections)?;
        encode_optional_selection(&mut writer, self.selection.as_ref())?;
        encode_count(&mut writer, "ordering", 0, MAX_ORDERING)?;
        let bytes = writer.finish();
        validate_artifact_size(bytes.len())?;
        Ok(bytes)
    }

    /// Decodes exactly one canonical version-3 `SELECT DISTINCT` artifact.
    pub fn decode(bytes: &[u8]) -> Result<Self, ServerPlanError> {
        let (mut reader, scan, projections, mut remaining_expression_nodes) =
            decode_plan_prefix(bytes, DISTINCT_FORMAT_VERSION)?;
        let selection = decode_optional_selection(&mut reader, &mut remaining_expression_nodes)?;
        let ordering_count = reader.u32()?;
        if ordering_count != 0 {
            return Err(ServerPlanError::DistinctOrderingNotAllowed {
                count: ordering_count,
            });
        }
        reader.require_finished()?;
        Self::new(scan, projections, selection)
    }
}
pub(super) fn encode_plan_prefix(
    version: u32,
    scan: Scan,
    projections: &[Expression],
) -> Result<Writer, ServerPlanError> {
    let mut writer = Writer::new();
    writer.bytes(&MAGIC);
    writer.u32(version);
    writer.u32(EXACT_INPUT_COUNT);
    encode_scan(&mut writer, scan);
    encode_count(
        &mut writer,
        "projections",
        projections.len(),
        MAX_PROJECTIONS,
    )?;
    for expression in projections {
        encode_expression(&mut writer, expression, 0)?;
    }
    Ok(writer)
}

fn decode_plan_prefix(
    bytes: &[u8],
    expected_version: u32,
) -> Result<(Reader<'_>, Scan, Vec<Expression>, u32), ServerPlanError> {
    validate_artifact_size(bytes.len())?;
    let mut reader = Reader::new(bytes);
    if reader.array::<8>()? != MAGIC {
        return Err(ServerPlanError::InvalidMagic);
    }
    let version = reader.u32()?;
    if version != expected_version {
        return Err(ServerPlanError::UnsupportedVersion(version));
    }
    let input_count = reader.u32()?;
    if input_count != EXACT_INPUT_COUNT {
        return Err(ServerPlanError::UnexpectedInputCount(input_count));
    }
    let scan = decode_scan(&mut reader)?;
    let projection_count = decode_count(&mut reader, "projections", MAX_PROJECTIONS)?;
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
    Ok((reader, scan, projections, remaining_expression_nodes))
}

pub(super) fn encode_optional_selection(
    writer: &mut Writer,
    selection: Option<&Expression>,
) -> Result<(), ServerPlanError> {
    match selection {
        Some(expression) => {
            writer.boolean(true);
            encode_expression(writer, expression, 0)?;
        }
        None => writer.boolean(false),
    }
    Ok(())
}

fn decode_optional_selection(
    reader: &mut Reader<'_>,
    remaining_expression_nodes: &mut u32,
) -> Result<Option<Expression>, ServerPlanError> {
    match decode_boolean(reader, "selection presence")? {
        true => decode_expression(reader, 0, remaining_expression_nodes).map(Some),
        false => Ok(None),
    }
}

fn validate_plan(plan: &ServerPlan) -> Result<(), ServerPlanError> {
    let mut remaining_expression_nodes = MAX_EXPRESSION_NODES;
    validate_scan_and_projections(
        plan.scan,
        &plan.projections,
        &mut remaining_expression_nodes,
    )?;
    validate_optional_selection(
        plan.selection.as_ref(),
        plan.scan,
        &mut remaining_expression_nodes,
    )?;
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

fn validate_identity_selected_plan(
    plan: &IdentitySelectedServerPlan,
) -> Result<(), ServerPlanError> {
    let mut remaining_expression_nodes = MAX_EXPRESSION_NODES;
    validate_scan_and_projections(
        plan.scan,
        &plan.projections,
        &mut remaining_expression_nodes,
    )?;
    consume_identity_selection_nodes(&mut remaining_expression_nodes)?;
    Ok(())
}

fn validate_distinct_plan(plan: &DistinctServerPlan) -> Result<(), ServerPlanError> {
    let mut remaining_expression_nodes = MAX_EXPRESSION_NODES;
    validate_scan_and_projections(
        plan.scan,
        &plan.projections,
        &mut remaining_expression_nodes,
    )?;
    validate_optional_selection(
        plan.selection.as_ref(),
        plan.scan,
        &mut remaining_expression_nodes,
    )?;
    for projection in &plan.projections {
        let resolved_type = projection.value_type.resolved_type;
        if !supports_distinct_projection(resolved_type) {
            return Err(ServerPlanError::UnsupportedDistinctProjectionType { resolved_type });
        }
    }
    Ok(())
}

fn validate_unique_text_selected_plan(
    plan: &UniqueTextSelectedServerPlan,
) -> Result<(), ServerPlanError> {
    let mut remaining_expression_nodes = MAX_EXPRESSION_NODES;
    validate_scan_and_projections(
        plan.scan,
        &plan.projections,
        &mut remaining_expression_nodes,
    )?;
    consume_unique_text_selection_nodes(&mut remaining_expression_nodes)?;
    validate_unique_text_selector(plan.scan, plan.selector)
}

fn validate_optional_selection(
    selection: Option<&Expression>,
    scan: Scan,
    remaining_expression_nodes: &mut u32,
) -> Result<(), ServerPlanError> {
    let Some(selection) = selection else {
        return Ok(());
    };
    validate_expression(selection, scan, 0, remaining_expression_nodes)?;
    if !matches!(
        project_legacy_resolved_type(selection.value_type.resolved_type),
        Some(LegacyResolvedType::Scalar(StandardScalar::Boolean))
    ) {
        return Err(ServerPlanError::InvalidModel(
            "a selection must have resolved BOOLEAN type",
        ));
    }
    Ok(())
}

const fn supports_distinct_projection(resolved_type: ResolvedType) -> bool {
    matches!(
        project_legacy_resolved_type(resolved_type),
        Some(LegacyResolvedType::Scalar(
            StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::BinaryLargeObject
        )) | Some(LegacyResolvedType::Reference(_))
    )
}

fn validate_scan_and_projections(
    scan: Scan,
    projections: &[Expression],
    remaining_expression_nodes: &mut u32,
) -> Result<(), ServerPlanError> {
    if scan.input != PRIMARY_INPUT {
        return Err(ServerPlanError::InvalidInputSlot(scan.input));
    }
    validate_count("projections", projections.len(), MAX_PROJECTIONS)?;
    if projections.is_empty() {
        return Err(ServerPlanError::InvalidModel(
            "a server plan must contain at least one projection",
        ));
    }
    for expression in projections {
        validate_expression(expression, scan, 0, remaining_expression_nodes)?;
    }
    Ok(())
}

fn consume_identity_selection_nodes(remaining_nodes: &mut u32) -> Result<(), ServerPlanError> {
    if *remaining_nodes < IDENTITY_SELECTION_EXPRESSION_NODES {
        return Err(ServerPlanError::ExpressionNodeLimitExceeded);
    }
    *remaining_nodes -= IDENTITY_SELECTION_EXPRESSION_NODES;
    Ok(())
}

fn consume_unique_text_selection_nodes(remaining_nodes: &mut u32) -> Result<(), ServerPlanError> {
    if *remaining_nodes < UNIQUE_TEXT_SELECTION_EXPRESSION_NODES {
        return Err(ServerPlanError::ExpressionNodeLimitExceeded);
    }
    *remaining_nodes -= UNIQUE_TEXT_SELECTION_EXPRESSION_NODES;
    Ok(())
}

fn validate_unique_text_selector(
    scan: Scan,
    selector: SelectBindValue,
) -> Result<(), ServerPlanError> {
    let SelectBindValue::Text {
        scan_object_type,
        field_owner,
        resolved_type,
        parameter_required_non_null,
        ..
    } = selector;
    if scan_object_type != scan.object_type {
        return Err(ServerPlanError::InvalidModel(
            "a unique-Text selector scan object must match the plan scan",
        ));
    }
    if field_owner != scan.object_type {
        return Err(ServerPlanError::InvalidModel(
            "a unique-Text selector field owner must match the plan scan",
        ));
    }
    if !matches!(
        resolved_type,
        ResolvedType::Scalar(StandardScalar::CharacterLargeObject) | ResolvedType::Value(_)
    ) {
        return Err(ServerPlanError::InvalidModel(
            "a unique-Text selector must use resolved TEXT type",
        ));
    }
    if !parameter_required_non_null {
        return Err(ServerPlanError::InvalidModel(
            "a unique-Text selector parameter must be required and non-null",
        ));
    }
    Ok(())
}

fn collect_projections(
    projections: impl IntoIterator<Item = Expression>,
) -> Result<Vec<Expression>, ServerPlanError> {
    let mut collected = Vec::new();
    for (index, projection) in projections.into_iter().enumerate() {
        let count = index.saturating_add(1);
        if count > MAX_PROJECTIONS as usize {
            return Err(ServerPlanError::CollectionLimit {
                kind: "projections",
                count: u32::try_from(count).unwrap_or(u32::MAX),
                maximum: MAX_PROJECTIONS,
            });
        }
        collected.push(projection);
    }
    Ok(collected)
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

fn identity_selector_value_type(scan: Scan) -> ValueType {
    ValueType {
        resolved_type: ResolvedType::reference(scan.object_type),
        nullable: false,
    }
}

pub(super) fn encode_identity_selection(
    writer: &mut Writer,
    scan: Scan,
    selector: IdentitySelector,
) -> Result<(), ServerPlanError> {
    writer.u8(4);
    encode_value_type(
        writer,
        ValueType {
            resolved_type: ResolvedType::scalar(StandardScalar::Boolean),
            nullable: false,
        },
    )?;
    writer.u8(1);
    encode_value_type(writer, identity_selector_value_type(scan))?;
    writer.u32(PRIMARY_INPUT);
    writer.u8(5);
    encode_value_type(writer, identity_selector_value_type(scan))?;
    writer.function_id(selector.owner);
    writer.parameter_id(selector.parameter);
    Ok(())
}

fn decode_identity_selection(
    reader: &mut Reader<'_>,
    scan: Scan,
) -> Result<IdentitySelector, ServerPlanError> {
    let equality_tag = reader.u8()?;
    let equality_type = decode_value_type(reader)?;
    let expected_boolean = ValueType {
        resolved_type: ResolvedType::scalar(StandardScalar::Boolean),
        nullable: false,
    };
    if equality_tag != 4 || equality_type != expected_boolean {
        return Err(ServerPlanError::InvalidModel(
            "an identity-selected server plan must use its fixed equality selection",
        ));
    }

    let object_reference_tag = reader.u8()?;
    let object_reference_type = decode_value_type(reader)?;
    let object_reference_input = reader.u32()?;
    if object_reference_tag != 1
        || object_reference_type != identity_selector_value_type(scan)
        || object_reference_input != PRIMARY_INPUT
    {
        return Err(ServerPlanError::InvalidModel(
            "an identity-selected server plan must compare the primary object reference first",
        ));
    }

    let parameter_tag = reader.u8()?;
    let parameter_type = decode_value_type(reader)?;
    if parameter_tag != 5 || parameter_type != identity_selector_value_type(scan) {
        return Err(ServerPlanError::InvalidModel(
            "an identity-selected server plan must compare its exact non-null selector parameter second",
        ));
    }
    Ok(IdentitySelector::new(
        reader.function_id()?,
        reader.parameter_id()?,
    ))
}

fn encode_unique_text_selection(
    writer: &mut Writer,
    selector: SelectBindValue,
) -> Result<(), ServerPlanError> {
    validate_unique_text_selector(
        Scan {
            input: PRIMARY_INPUT,
            object_type: match selector {
                SelectBindValue::Text {
                    scan_object_type, ..
                } => scan_object_type,
            },
        },
        selector,
    )?;
    let SelectBindValue::Text {
        scan_object_type,
        field_owner,
        field,
        parameter_owner,
        parameter,
        resolved_type,
        field_nullable,
        parameter_required_non_null,
    } = selector;
    writer.u8(1);
    writer.type_id(scan_object_type);
    writer.type_id(field_owner);
    writer.field_id(field);
    writer.function_id(parameter_owner);
    writer.parameter_id(parameter);
    encode_unique_text_resolved_type(writer, resolved_type)?;
    writer.boolean(field_nullable);
    writer.boolean(parameter_required_non_null);
    Ok(())
}

fn decode_unique_text_selection(
    reader: &mut Reader<'_>,
) -> Result<SelectBindValue, ServerPlanError> {
    let tag = reader.u8()?;
    if tag != 1 {
        return Err(ServerPlanError::InvalidEnumTag {
            kind: "unique-Text selector",
            tag,
        });
    }
    Ok(SelectBindValue::Text {
        scan_object_type: reader.type_id()?,
        field_owner: reader.type_id()?,
        field: reader.field_id()?,
        parameter_owner: reader.function_id()?,
        parameter: reader.parameter_id()?,
        resolved_type: decode_unique_text_resolved_type(reader)?,
        field_nullable: decode_boolean(reader, "unique-Text selector field nullability")?,
        parameter_required_non_null: decode_boolean(
            reader,
            "unique-Text selector parameter required non-null",
        )?,
    })
}

fn encode_unique_text_resolved_type(
    writer: &mut Writer,
    resolved_type: ResolvedType,
) -> Result<(), ServerPlanError> {
    match resolved_type {
        ResolvedType::Scalar(StandardScalar::CharacterLargeObject) => {
            writer.u8(1);
            writer.u8(encode_standard_scalar(StandardScalar::CharacterLargeObject));
            Ok(())
        }
        ResolvedType::Value(type_id) => {
            writer.u8(4);
            writer.type_id(type_id);
            Ok(())
        }
        _ => Err(ServerPlanError::InvalidModel(
            "a unique-Text selector must use resolved TEXT type",
        )),
    }
}

fn decode_unique_text_resolved_type(
    reader: &mut Reader<'_>,
) -> Result<ResolvedType, ServerPlanError> {
    match reader.u8()? {
        1 => {
            let scalar = decode_standard_scalar(reader.u8()?)?;
            if scalar != StandardScalar::CharacterLargeObject {
                return Err(ServerPlanError::InvalidModel(
                    "a unique-Text selector must use resolved TEXT type",
                ));
            }
            Ok(ResolvedType::scalar(scalar))
        }
        4 => Ok(ResolvedType::Value(reader.type_id()?)),
        tag => Err(ServerPlanError::InvalidEnumTag {
            kind: "unique-Text selector resolved type",
            tag,
        }),
    }
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
            encode_value_type(writer, expression.value_type)?;
            writer.u32(*input);
        }
        ExpressionKind::FieldPath { input, steps } => {
            writer.u8(2);
            encode_value_type(writer, expression.value_type)?;
            writer.u32(*input);
            encode_count(
                writer,
                "field path steps",
                steps.len(),
                MAX_FIELD_PATH_STEPS,
            )?;
            for step in steps {
                writer.type_id(step.owner);
                writer.field_id(step.field);
            }
        }
        ExpressionKind::BooleanLiteral { value } => {
            writer.u8(3);
            encode_value_type(writer, expression.value_type)?;
            writer.boolean(*value);
        }
        ExpressionKind::Equality { left, right } => {
            writer.u8(4);
            encode_value_type(writer, expression.value_type)?;
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
            let count = decode_count(reader, "field path steps", MAX_FIELD_PATH_STEPS)?;
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
            value: decode_boolean(reader, "literal")?,
        },
        4 => ExpressionKind::Equality {
            left: Box::new(decode_expression(reader, depth + 1, remaining_nodes)?),
            right: Box::new(decode_expression(reader, depth + 1, remaining_nodes)?),
        },
        _ => unreachable!("expression tag was validated before its payload"),
    };
    Ok(Expression { kind, value_type })
}

fn encode_value_type(writer: &mut Writer, value_type: ValueType) -> Result<(), ServerPlanError> {
    encode_resolved_type(writer, value_type.resolved_type)?;
    writer.boolean(value_type.nullable);
    Ok(())
}

fn decode_value_type(reader: &mut Reader<'_>) -> Result<ValueType, ServerPlanError> {
    Ok(ValueType {
        resolved_type: decode_resolved_type(reader)?,
        nullable: decode_boolean(reader, "expression nullability")?,
    })
}

pub(super) fn encode_resolved_type(
    writer: &mut Writer,
    resolved_type: ResolvedType,
) -> Result<(), ServerPlanError> {
    match project_legacy_resolved_type(resolved_type) {
        Some(LegacyResolvedType::Scalar(scalar)) => {
            writer.u8(1);
            writer.u8(encode_standard_scalar(scalar));
        }
        Some(LegacyResolvedType::Named(type_id)) => {
            writer.u8(2);
            writer.type_id(type_id);
        }
        Some(LegacyResolvedType::Reference(target)) => {
            writer.u8(3);
            writer.type_id(target);
        }
        None => {
            return Err(ServerPlanError::InvalidModel(
                "a server plan value type must use a supported legacy resolved type",
            ));
        }
    }
    Ok(())
}

pub(super) fn decode_resolved_type(
    reader: &mut Reader<'_>,
) -> Result<ResolvedType, ServerPlanError> {
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

pub(super) fn encode_count(
    writer: &mut Writer,
    kind: &'static str,
    count: usize,
    maximum: u32,
) -> Result<(), ServerPlanError> {
    writer.u32(validate_count(kind, count, maximum)?);
    Ok(())
}

fn decode_boolean(reader: &mut Reader<'_>, context: &'static str) -> Result<bool, ServerPlanError> {
    match reader.u8()? {
        0 => Ok(false),
        1 => Ok(true),
        value => Err(ServerPlanError::InvalidBoolean { context, value }),
    }
}

fn decode_count(
    reader: &mut Reader<'_>,
    kind: &'static str,
    maximum: u32,
) -> Result<u32, ServerPlanError> {
    let count = reader.u32()?;
    if count > maximum {
        return Err(ServerPlanError::CollectionLimit {
            kind,
            count,
            maximum,
        });
    }
    Ok(count)
}
