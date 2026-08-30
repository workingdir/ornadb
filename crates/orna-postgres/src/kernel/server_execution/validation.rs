use super::*;

pub(super) fn validate_function_signature(
    function: &FunctionDefinition,
) -> Result<(), PostgresKernelError> {
    if function.domain() != FunctionDomain::Server {
        return Err(server_error(ServerSelectError::FunctionDomain {
            function: function.id(),
        }));
    }
    if !function.parameters().is_empty() {
        return Err(server_error(ServerSelectError::FunctionSignature {
            function: function.id(),
            rule: "SERVER SELECT functions must have zero parameters",
        }));
    }
    if !matches!(function.return_type(), FunctionReturn::Rows(columns) if !columns.is_empty()) {
        return Err(server_error(ServerSelectError::FunctionSignature {
            function: function.id(),
            rule: "SERVER SELECT functions must return nonempty ROWS",
        }));
    }
    if function.security() != FunctionSecurity::Invoker {
        return Err(server_error(ServerSelectError::FunctionSignature {
            function: function.id(),
            rule: "SERVER SELECT functions must use INVOKER security",
        }));
    }
    if !matches!(
        function.transaction(),
        None | Some(FunctionTransaction::Atomic | FunctionTransaction::ReadOnly)
    ) {
        return Err(server_error(ServerSelectError::FunctionSignature {
            function: function.id(),
            rule: "SERVER SELECT functions cannot use MANUAL transactions",
        }));
    }
    Ok(())
}

pub(super) fn validate_identity_selected_function_signature(
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
    function: &FunctionDefinition,
) -> Result<(), PostgresKernelError> {
    if function.domain() != FunctionDomain::Server {
        return Err(server_error(ServerSelectError::FunctionDomain {
            function: function.id(),
        }));
    }
    if !matches!(function.return_type(), FunctionReturn::Rows(columns) if !columns.is_empty()) {
        return Err(function_signature_error(
            function.id(),
            "SERVER SELECT functions must return nonempty ROWS",
        ));
    }
    if function.security() != FunctionSecurity::Invoker {
        return Err(function_signature_error(
            function.id(),
            "parameterised SERVER SELECT functions must use INVOKER security",
        ));
    }
    if function.transaction() != Some(FunctionTransaction::ReadOnly) {
        return Err(function_signature_error(
            function.id(),
            "parameterised SERVER SELECT functions must use READ ONLY transactions",
        ));
    }
    if function.volatility() != FunctionVolatility::Stable {
        return Err(function_signature_error(
            function.id(),
            "parameterised SERVER SELECT functions must use STABLE volatility",
        ));
    }
    let [parameter] = function.parameters() else {
        return Err(function_signature_error(
            function.id(),
            "parameterised SERVER SELECT functions must declare exactly one parameter",
        ));
    };
    if parameter.default_expression().is_some() {
        return Err(function_signature_error(
            function.id(),
            "the identity selector parameter cannot have a default expression",
        ));
    }
    let Some(target) = parameter.resolved_type().reference_target() else {
        return Err(function_signature_error(
            function.id(),
            "the selector parameter must use REF to an available object type",
        ));
    };
    if catalogue.object_type_by_id(target).is_none() {
        return Err(function_signature_error(
            function.id(),
            "the selector parameter must use REF to an available object type",
        ));
    }
    Ok(())
}

pub(super) fn validate_unique_text_selected_function_signature(
    function: &FunctionDefinition,
) -> Result<(), PostgresKernelError> {
    if function.domain() != FunctionDomain::Server {
        return Err(server_error(ServerSelectError::FunctionDomain {
            function: function.id(),
        }));
    }
    if !matches!(function.return_type(), FunctionReturn::Rows(columns) if !columns.is_empty()) {
        return Err(function_signature_error(
            function.id(),
            "unique-Text-selected SERVER functions must return nonempty ROWS",
        ));
    }
    if function.security() != FunctionSecurity::Invoker {
        return Err(function_signature_error(
            function.id(),
            "unique-Text-selected SERVER functions must use INVOKER security",
        ));
    }
    if function.transaction() != Some(FunctionTransaction::ReadOnly) {
        return Err(function_signature_error(
            function.id(),
            "unique-Text-selected SERVER functions must use READ ONLY transactions",
        ));
    }
    if function.volatility() != FunctionVolatility::Stable {
        return Err(function_signature_error(
            function.id(),
            "unique-Text-selected SERVER functions must use STABLE volatility",
        ));
    }
    let [parameter] = function.parameters() else {
        return Err(function_signature_error(
            function.id(),
            "unique-Text-selected SERVER functions must declare exactly one parameter",
        ));
    };
    if parameter.default_expression().is_some() {
        return Err(function_signature_error(
            function.id(),
            "the unique-Text selector parameter cannot have a default expression",
        ));
    }
    Ok(())
}

pub(super) fn validate_distinct_function_signature(
    function: &FunctionDefinition,
) -> Result<(), PostgresKernelError> {
    if function.domain() != FunctionDomain::Server {
        return Err(server_error(ServerSelectError::FunctionDomain {
            function: function.id(),
        }));
    }
    if !function.parameters().is_empty() {
        return Err(function_signature_error(
            function.id(),
            "SELECT DISTINCT SERVER functions must have zero parameters",
        ));
    }
    if !matches!(function.return_type(), FunctionReturn::Rows(columns) if !columns.is_empty()) {
        return Err(function_signature_error(
            function.id(),
            "SELECT DISTINCT SERVER functions must return nonempty ROWS",
        ));
    }
    if function.security() != FunctionSecurity::Invoker {
        return Err(function_signature_error(
            function.id(),
            "SELECT DISTINCT SERVER functions must use INVOKER security",
        ));
    }
    if function.transaction() != Some(FunctionTransaction::ReadOnly) {
        return Err(function_signature_error(
            function.id(),
            "SELECT DISTINCT SERVER functions must use READ ONLY transactions",
        ));
    }
    if function.volatility() != FunctionVolatility::Stable {
        return Err(function_signature_error(
            function.id(),
            "SELECT DISTINCT SERVER functions must use STABLE volatility",
        ));
    }
    Ok(())
}

pub(super) fn validate_identity_selected_plan(
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
    context: &CatalogueHashContext,
    function: &FunctionDefinition,
    plan: &IdentitySelectedServerPlan,
) -> Result<(), PostgresKernelError> {
    validate_execution_complexity_for_projections(plan.projections())?;
    let scan = plan.scan();
    if scan.input != 0 || catalogue.object_type_by_id(scan.object_type).is_none() {
        return Err(plan_invariant(
            "scan must use active input zero and an active object type",
        ));
    }
    let FunctionReturn::Rows(return_columns) = function.return_type() else {
        return Err(plan_invariant("function return shape must be ROWS"));
    };
    if plan.projections().len() != return_columns.len() {
        return Err(plan_invariant(
            "projection count must equal ROWS column count",
        ));
    }
    for (projection, column) in plan.projections().iter().zip(return_columns) {
        validate_expression_with_equality_rule(
            catalogue,
            context,
            scan.object_type,
            projection,
            PARAMETERISED_EQUALITY_RULE,
        )?;
        if !runtime_types_match(
            context,
            projection.value_type.resolved_type,
            column.resolved_type(),
        ) {
            return Err(plan_invariant("projection type must equal its ROWS column"));
        }
        if !supports_result_type(
            catalogue,
            context,
            projection.value_type.resolved_type,
            projection.value_type.nullable,
        ) {
            return Err(plan_invariant(
                "projection type is outside the initial runtime result subset",
            ));
        }
    }
    let selector = plan.selector();
    let [parameter] = function.parameters() else {
        return Err(plan_invariant(
            "parameterised SERVER SELECT function must have one declared selector parameter",
        ));
    };
    if selector.owner() != function.id() || selector.parameter() != parameter.id() {
        return Err(plan_invariant(
            "identity selector owner and parameter must equal the active function signature",
        ));
    }
    if parameter.resolved_type() != ResolvedType::reference(scan.object_type) {
        return Err(plan_invariant(
            "the selector parameter must use REF to the object type selected in FROM",
        ));
    }
    Ok(())
}

pub(super) fn validate_unique_text_selected_plan(
    catalogue: &CatalogueSnapshot,
    context: &CatalogueHashContext,
    function: &FunctionDefinition,
    plan: &UniqueTextSelectedServerPlan,
) -> Result<(), PostgresKernelError> {
    validate_execution_complexity_for_projections(plan.projections())?;
    let scan = plan.scan();
    let Some(object_type) = catalogue.object_type_by_id(scan.object_type) else {
        return Err(plan_invariant(
            "unique-Text selector scan must use active input zero and an active object type",
        ));
    };
    if scan.input != 0 {
        return Err(plan_invariant(
            "unique-Text selector scan must use active input zero and an active object type",
        ));
    }
    let FunctionReturn::Rows(return_columns) = function.return_type() else {
        return Err(plan_invariant("function return shape must be ROWS"));
    };
    if plan.projections().len() != return_columns.len() {
        return Err(plan_invariant(
            "projection count must equal ROWS column count",
        ));
    }
    for (projection, column) in plan.projections().iter().zip(return_columns) {
        validate_expression_with_equality_rule(
            catalogue,
            context,
            scan.object_type,
            projection,
            PARAMETERISED_EQUALITY_RULE,
        )?;
        if !runtime_types_match(
            context,
            projection.value_type.resolved_type,
            column.resolved_type(),
        ) {
            return Err(plan_invariant("projection type must equal its ROWS column"));
        }
        if !supports_result_type(
            catalogue,
            context,
            projection.value_type.resolved_type,
            projection.value_type.nullable,
        ) {
            return Err(plan_invariant(
                "projection type is outside the initial runtime result subset",
            ));
        }
    }
    let UniqueTextSelectBindValue::Text {
        scan_object_type,
        field_owner,
        field,
        parameter_owner,
        parameter,
        resolved_type,
        field_nullable,
        parameter_required_non_null,
    } = plan.selector();
    if *scan_object_type != scan.object_type || *field_owner != scan.object_type {
        return Err(plan_invariant(
            "unique-Text selector scan and direct field owner must match the active scan",
        ));
    }
    let field = object_type.field_by_id(*field).ok_or_else(|| {
        plan_invariant("unique-Text selector field must exist on the active scanned object type")
    })?;
    if !field.unique()
        || field.nullable() != *field_nullable
        || field.resolved_type() != *resolved_type
        || !supports_unique_text(context, field.resolved_type())
    {
        return Err(plan_invariant(
            "unique-Text selector field must be an exact active nullable or required unique Text field",
        ));
    }
    let [declared_parameter] = function.parameters() else {
        return Err(plan_invariant(
            "unique-Text-selected SERVER function must have one declared selector parameter",
        ));
    };
    if *parameter_owner != function.id()
        || *parameter != declared_parameter.id()
        || !*parameter_required_non_null
        || declared_parameter.default_expression().is_some()
        || declared_parameter.resolved_type() != *resolved_type
        || !supports_unique_text(context, declared_parameter.resolved_type())
    {
        return Err(plan_invariant(
            "unique-Text selector owner, parameter, required fact, and exact Text authority must match the active function signature",
        ));
    }
    Ok(())
}

pub(super) fn validate_distinct_plan(
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
    context: &CatalogueHashContext,
    function: &FunctionDefinition,
    plan: &DistinctServerPlan,
) -> Result<(), PostgresKernelError> {
    validate_execution_complexity_for_distinct(plan)?;
    let scan = plan.scan();
    if scan.input != 0 || catalogue.object_type_by_id(scan.object_type).is_none() {
        return Err(plan_invariant(
            "scan must use active input zero and an active object type",
        ));
    }
    let FunctionReturn::Rows(return_columns) = function.return_type() else {
        return Err(plan_invariant("function return shape must be ROWS"));
    };
    if plan.projections().len() != return_columns.len() {
        return Err(plan_invariant(
            "projection count must equal ROWS column count",
        ));
    }
    for (projection, column) in plan.projections().iter().zip(return_columns) {
        validate_expression_with_equality_rule(
            catalogue,
            context,
            scan.object_type,
            projection,
            DISTINCT_EQUALITY_RULE,
        )?;
        if !runtime_types_match(
            context,
            projection.value_type.resolved_type,
            column.resolved_type(),
        ) {
            return Err(plan_invariant("projection type must equal its ROWS column"));
        }
        if !supports_distinct_projection_type(context, projection.value_type.resolved_type) {
            return Err(distinct_error(DISTINCT_PROJECTION_RULE));
        }
        if !supports_result_type(
            catalogue,
            context,
            projection.value_type.resolved_type,
            projection.value_type.nullable,
        ) {
            return Err(plan_invariant(
                "projection type is outside the initial runtime result subset",
            ));
        }
    }
    if let Some(selection) = plan.selection() {
        validate_expression_with_equality_rule(
            catalogue,
            context,
            scan.object_type,
            selection,
            DISTINCT_EQUALITY_RULE,
        )?;
        if selection.value_type.resolved_type != ResolvedType::scalar(StandardScalar::Boolean) {
            return Err(plan_invariant("selection must have BOOLEAN type"));
        }
    }
    Ok(())
}

fn validate_execution_complexity_for_projections(
    projections: &[Expression],
) -> Result<(), PostgresKernelError> {
    validate_expression_complexity(projections.iter())
}

fn validate_execution_complexity_for_distinct(
    plan: &DistinctServerPlan,
) -> Result<(), PostgresKernelError> {
    validate_expression_complexity(plan.projections().iter().chain(plan.selection()))
}

fn validate_expression_complexity<'a>(
    expressions: impl Iterator<Item = &'a Expression>,
) -> Result<(), PostgresKernelError> {
    let mut steps = 0usize;
    let mut binds = 0usize;
    for expression in expressions {
        count_expression_complexity(expression, &mut steps, &mut binds)?;
    }
    if steps > FIELD_PATH_STEP_LIMIT {
        return Err(server_error(ServerSelectError::ComplexityLimit {
            category: "field path steps",
            maximum: FIELD_PATH_STEP_LIMIT,
        }));
    }
    if binds > server_plan::MAX_EXPRESSION_NODES as usize {
        return Err(server_error(ServerSelectError::ComplexityLimit {
            category: "boolean binds",
            maximum: server_plan::MAX_EXPRESSION_NODES as usize,
        }));
    }
    Ok(())
}

pub(super) fn validate_identity_selected_arguments(
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
    context: &CatalogueHashContext,
    function: &FunctionDefinition,
    plan: &IdentitySelectedServerPlan,
    arguments: &[FunctionArgument],
) -> Result<ObjectId, PostgresKernelError> {
    let mut supplied = BTreeMap::new();
    for argument in arguments {
        let parameter_id = argument.parameter();
        if supplied.insert(parameter_id, argument.value()).is_some() {
            return Err(argument_error(
                Some(parameter_id),
                "the same parameter was supplied twice",
            ));
        }
        let parameter = function.parameter_by_id(parameter_id).ok_or_else(|| {
            argument_error(
                Some(parameter_id),
                "an argument was supplied for a parameter that this function does not declare",
            )
        })?;
        let value = argument.value();
        if value.is_null() {
            return Err(argument_error(
                Some(parameter_id),
                "function arguments cannot be NULL",
            ));
        }
        let RuntimeType::Flat(value_type) = value.runtime_type() else {
            return Err(argument_error(
                Some(parameter_id),
                "the argument uses an unsupported type or refers to an unavailable object type",
            ));
        };
        if !runtime_type_is_active(catalogue, context, value_type) {
            return Err(argument_error(
                Some(parameter_id),
                "the argument uses an unsupported type or refers to an unavailable object type",
            ));
        }
        if !runtime_types_match(context, value_type, parameter.resolved_type()) {
            return Err(argument_error(
                Some(parameter_id),
                "the argument type does not match the declared parameter type",
            ));
        }
    }
    let selector = plan.selector();
    let value = supplied.get(&selector.parameter()).ok_or_else(|| {
        argument_error(Some(selector.parameter()), "a required argument is missing")
    })?;
    match value {
        RuntimeValue::Reference { target, object } if *target == plan.scan().object_type => {
            Ok(*object)
        }
        _ => Err(argument_error(
            Some(selector.parameter()),
            "the selector argument must refer to the object type selected by this function",
        )),
    }
}

pub(super) fn validate_unique_text_selected_arguments(
    catalogue: &CatalogueSnapshot,
    context: &CatalogueHashContext,
    function: &FunctionDefinition,
    plan: &UniqueTextSelectedServerPlan,
    arguments: &[FunctionArgument],
) -> Result<String, PostgresKernelError> {
    let [argument] = arguments else {
        return Err(argument_error(
            None,
            "unique-Text-selected SERVER calls require exactly one Text argument",
        ));
    };
    let UniqueTextSelectBindValue::Text {
        parameter,
        resolved_type,
        ..
    } = plan.selector();
    if argument.parameter() != *parameter {
        return Err(argument_error(
            Some(argument.parameter()),
            "the supplied argument must name the unique-Text selector parameter",
        ));
    }
    let parameter = function
        .parameter_by_id(argument.parameter())
        .ok_or_else(|| {
            argument_error(
                Some(argument.parameter()),
                "an argument was supplied for a parameter that this function does not declare",
            )
        })?;
    if parameter.resolved_type() != *resolved_type
        || !supports_unique_text(context, parameter.resolved_type())
    {
        return Err(argument_error(
            Some(argument.parameter()),
            "the unique-Text selector parameter must retain exact active Text authority",
        ));
    }
    let RuntimeType::Flat(value_type) = argument.value().runtime_type() else {
        return Err(argument_error(
            Some(argument.parameter()),
            "the unique-Text selector argument must be one non-null Text value",
        ));
    };
    if !runtime_type_is_active(catalogue, context, value_type)
        || !runtime_types_match(context, value_type, parameter.resolved_type())
    {
        return Err(argument_error(
            Some(argument.parameter()),
            "the unique-Text selector argument type does not match the declared parameter type",
        ));
    }
    let RuntimeValue::Text(value) = argument.value() else {
        return Err(argument_error(
            Some(argument.parameter()),
            "the unique-Text selector argument must be one non-null Text value",
        ));
    };
    if value.contains('\0') {
        return Err(argument_error(
            Some(argument.parameter()),
            "unique-Text selector arguments cannot contain U+0000",
        ));
    }
    Ok(value.clone())
}

pub(super) fn validate_no_arguments(
    arguments: &[FunctionArgument],
) -> Result<(), PostgresKernelError> {
    if arguments.is_empty() {
        Ok(())
    } else {
        Err(argument_error(
            None,
            "this function does not accept arguments",
        ))
    }
}

fn runtime_type_is_active(
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
    context: &CatalogueHashContext,
    resolved_type: ResolvedType,
) -> bool {
    match resolve_catalogue_runtime_type(catalogue, context, resolved_type) {
        runtime @ ResolvedRuntimeType::LegacyScalar(_)
        | runtime @ ResolvedRuntimeType::VerifiedValue { .. } => postgres_type(runtime).is_some(),
        ResolvedRuntimeType::CatalogueEnum(_) => true,
        ResolvedRuntimeType::Record(_) => false,
        ResolvedRuntimeType::Reference(target) => catalogue.object_type_by_id(target).is_some(),
        ResolvedRuntimeType::Unsupported => false,
    }
}

pub(super) fn function_signature_error(
    function: FunctionId,
    rule: &'static str,
) -> PostgresKernelError {
    server_error(ServerSelectError::FunctionSignature { function, rule })
}

pub(super) fn argument_error(
    parameter: Option<ParameterId>,
    rule: &'static str,
) -> PostgresKernelError {
    server_error(ServerSelectError::Argument { parameter, rule })
}

pub(super) fn artifact_error(function: FunctionId, rule: &'static str) -> PostgresKernelError {
    server_error(ServerSelectError::Artifact { function, rule })
}

pub(super) fn distinct_error(rule: &'static str) -> PostgresKernelError {
    server_error(ServerSelectError::Distinct { rule })
}

pub(super) fn validate_plan(
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    plan: &ServerPlan,
) -> Result<(), PostgresKernelError> {
    let catalogue = active.catalogue();
    let context = active.catalogue_hash_context();
    validate_execution_complexity(plan)?;
    if plan.scan.input != 0 || catalogue.object_type_by_id(plan.scan.object_type).is_none() {
        return Err(plan_invariant(
            "scan must use active input zero and an active object type",
        ));
    }
    let FunctionReturn::Rows(return_columns) = function.return_type() else {
        return Err(plan_invariant("function return shape must be ROWS"));
    };
    if plan.projections.len() != return_columns.len() {
        return Err(plan_invariant(
            "projection count must equal ROWS column count",
        ));
    }
    for (projection, column) in plan.projections.iter().zip(return_columns) {
        validate_expression(catalogue, context, plan.scan.object_type, projection)?;
        if !runtime_types_match(
            context,
            projection.value_type.resolved_type,
            column.resolved_type(),
        ) {
            return Err(plan_invariant("projection type must equal its ROWS column"));
        }
        if !supports_result_type(
            catalogue,
            context,
            projection.value_type.resolved_type,
            projection.value_type.nullable,
        ) {
            return Err(plan_invariant(
                "projection type is outside the initial runtime result subset",
            ));
        }
    }
    if let Some(selection) = &plan.selection {
        validate_expression(catalogue, context, plan.scan.object_type, selection)?;
        if selection.value_type.resolved_type != ResolvedType::scalar(StandardScalar::Boolean) {
            return Err(plan_invariant("selection must have BOOLEAN type"));
        }
    }
    for ordering in &plan.ordering {
        validate_expression(
            catalogue,
            context,
            plan.scan.object_type,
            &ordering.expression,
        )?;
        if !supports_ordering_type(context, ordering.expression.value_type.resolved_type) {
            return Err(plan_invariant(
                "version 1 SERVER SELECT ordering supports only INTEGER and BIGINT",
            ));
        }
    }
    Ok(())
}

fn validate_expression(
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
    context: &CatalogueHashContext,
    scan: TypeId,
    expression: &Expression,
) -> Result<(), PostgresKernelError> {
    validate_expression_with_equality_rule(
        catalogue,
        context,
        scan,
        expression,
        VERSION_ONE_EQUALITY_RULE,
    )
}

fn validate_expression_with_equality_rule(
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
    context: &CatalogueHashContext,
    scan: TypeId,
    expression: &Expression,
    equality_rule: &'static str,
) -> Result<(), PostgresKernelError> {
    match &expression.kind {
        ExpressionKind::ObjectReference { input } => {
            if *input != 0
                || expression.value_type.resolved_type != ResolvedType::reference(scan)
                || expression.value_type.nullable
            {
                return Err(plan_invariant(
                    "object reference must be non-nullable input zero of the scan type",
                ));
            }
        }
        ExpressionKind::FieldPath { input, steps } => {
            if *input != 0 {
                return Err(plan_invariant("field path must use input zero"));
            }
            let (resolved_type, nullable) = field_path_type(catalogue, scan, steps)?;
            if !runtime_types_match(context, expression.value_type.resolved_type, resolved_type)
                || expression.value_type.nullable != nullable
            {
                return Err(plan_invariant(
                    "field path result type and nullability must match every active hop",
                ));
            }
        }
        ExpressionKind::BooleanLiteral { .. } => {
            if expression.value_type.resolved_type != ResolvedType::scalar(StandardScalar::Boolean)
                || expression.value_type.nullable
            {
                return Err(plan_invariant(
                    "BOOLEAN literal must have non-nullable BOOLEAN type",
                ));
            }
        }
        ExpressionKind::Equality { left, right } => {
            validate_expression_with_equality_rule(catalogue, context, scan, left, equality_rule)?;
            validate_expression_with_equality_rule(catalogue, context, scan, right, equality_rule)?;
            if left.value_type.resolved_type != right.value_type.resolved_type
                || expression.value_type.resolved_type
                    != ResolvedType::scalar(StandardScalar::Boolean)
                || expression.value_type.nullable
                    != (left.value_type.nullable || right.value_type.nullable)
            {
                return Err(plan_invariant(
                    "equality operands and nullable BOOLEAN result must match",
                ));
            }
            if !supports_equality_type(context, left.value_type.resolved_type) {
                return Err(plan_invariant(equality_rule));
            }
        }
    }
    Ok(())
}

pub(super) fn supports_ordering_type(
    context: &CatalogueHashContext,
    resolved_type: ResolvedType,
) -> bool {
    matches!(
        resolve_runtime_type(context, resolved_type).compatibility_scalar(),
        Some(StandardScalar::Integer | StandardScalar::BigInt)
    )
}

pub(super) fn supports_equality_type(
    context: &CatalogueHashContext,
    resolved_type: ResolvedType,
) -> bool {
    matches!(
        resolve_runtime_type(context, resolved_type).compatibility_scalar(),
        Some(
            StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::BinaryLargeObject
        )
    ) || matches!(
        resolve_runtime_type(context, resolved_type),
        ResolvedRuntimeType::Reference(_)
    )
}

fn supports_unique_text(context: &CatalogueHashContext, resolved_type: ResolvedType) -> bool {
    match (context.standard(), resolved_type) {
        (None, ResolvedType::Scalar(StandardScalar::CharacterLargeObject)) => true,
        (Some(standard), ResolvedType::Value(type_id)) => standard
            .catalogue()
            .value_type_by_id(type_id)
            .is_some_and(|value_type| {
                value_type.kind() == ValueTypeKind::Primitive
                    && value_type.mutability() == ValueTypeMutability::Immutable
                    && value_type.persistence() == ValueTypePersistence::Persistable
                    && value_type.representation_contract()
                        == "orna.kernel.value.character-large-object@1"
            }),
        _ => false,
    }
}

pub(super) fn supports_distinct_projection_type(
    context: &CatalogueHashContext,
    resolved_type: ResolvedType,
) -> bool {
    matches!(
        resolve_runtime_type(context, resolved_type).compatibility_scalar(),
        Some(
            StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::BinaryLargeObject
        )
    ) || matches!(
        resolve_runtime_type(context, resolved_type),
        ResolvedRuntimeType::Reference(_)
    )
}

pub(super) fn supports_result_type(
    catalogue: &CatalogueSnapshot,
    context: &CatalogueHashContext,
    resolved_type: ResolvedType,
    nullable: bool,
) -> bool {
    if nullable
        && matches!(
            resolve_catalogue_runtime_type(catalogue, context, resolved_type),
            ResolvedRuntimeType::Record(_)
        )
    {
        return false;
    }
    matches!(
        resolve_runtime_type(context, resolved_type).compatibility_scalar(),
        Some(
            StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::Float
                | StandardScalar::CharacterLargeObject
                | StandardScalar::BinaryLargeObject
        )
    ) || matches!(
        resolve_catalogue_runtime_type(catalogue, context, resolved_type),
        ResolvedRuntimeType::CatalogueEnum(_)
            | ResolvedRuntimeType::Record(_)
            | ResolvedRuntimeType::Reference(_)
    )
}

fn validate_execution_complexity(plan: &ServerPlan) -> Result<(), PostgresKernelError> {
    validate_expression_complexity(
        plan.projections
            .iter()
            .chain(plan.selection.iter())
            .chain(plan.ordering.iter().map(|ordering| &ordering.expression)),
    )
}

pub(super) fn validate_target_entries(
    catalogue: &CatalogueSnapshot,
    context: &CatalogueHashContext,
    projections: usize,
    columns: &[ResultColumn],
    ordering: usize,
) -> Result<(), PostgresKernelError> {
    let guards = columns
        .iter()
        .filter(|column| is_variable_type(catalogue, context, column.resolved_type()))
        .count();
    validate_target_entry_count(projections, guards, ordering)
}

pub(super) fn validate_target_entry_count(
    projections: usize,
    guards: usize,
    ordering: usize,
) -> Result<(), PostgresKernelError> {
    let entries = projections
        .checked_add(guards)
        .and_then(|entries| entries.checked_add(ordering))
        .ok_or_else(|| {
            server_error(ServerSelectError::ComplexityLimit {
                category: "generated PostgreSQL target entries",
                maximum: TARGET_ENTRY_LIMIT,
            })
        })?;
    if entries > TARGET_ENTRY_LIMIT {
        return Err(server_error(ServerSelectError::ComplexityLimit {
            category: "generated PostgreSQL target entries",
            maximum: TARGET_ENTRY_LIMIT,
        }));
    }
    Ok(())
}

fn count_expression_complexity(
    expression: &Expression,
    steps: &mut usize,
    binds: &mut usize,
) -> Result<(), PostgresKernelError> {
    match &expression.kind {
        ExpressionKind::ObjectReference { .. } => {}
        ExpressionKind::FieldPath { steps: path, .. } => {
            *steps = steps.checked_add(path.len()).ok_or_else(|| {
                server_error(ServerSelectError::ComplexityLimit {
                    category: "field path steps",
                    maximum: FIELD_PATH_STEP_LIMIT,
                })
            })?;
        }
        ExpressionKind::BooleanLiteral { .. } => {
            *binds = binds.checked_add(1).ok_or_else(|| {
                server_error(ServerSelectError::ComplexityLimit {
                    category: "boolean binds",
                    maximum: server_plan::MAX_EXPRESSION_NODES as usize,
                })
            })?;
        }
        ExpressionKind::Equality { left, right } => {
            count_expression_complexity(left, steps, binds)?;
            count_expression_complexity(right, steps, binds)?;
        }
    }
    Ok(())
}

pub(super) fn field_path_type(
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
    scan: TypeId,
    steps: &[FieldStep],
) -> Result<(ResolvedType, bool), PostgresKernelError> {
    let mut owner = scan;
    let mut nullable = false;
    for (index, step) in steps.iter().enumerate() {
        if step.owner != owner {
            return Err(plan_invariant(
                "field path owner must match the active reference hop",
            ));
        }
        let field = catalogue
            .object_type_by_id(owner)
            .and_then(|object| object.field_by_id(step.field))
            .ok_or_else(|| plan_invariant("field path field must exist on its active owner"))?;
        nullable |= field.nullable();
        if index + 1 == steps.len() {
            if let Some(target) = field.resolved_type().reference_target()
                && catalogue.object_type_by_id(target).is_none()
            {
                return Err(plan_invariant(
                    "final reference field path target must be an active object type",
                ));
            }
            return Ok((field.resolved_type(), nullable));
        }
        let Some(target) = field.resolved_type().reference_target() else {
            return Err(plan_invariant(
                "each non-final field path hop must be an object reference",
            ));
        };
        owner = target;
    }
    Err(plan_invariant("field path must contain at least one field"))
}

pub(super) fn validate_reference_evidence(
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    plan: &ServerPlan,
) -> Result<(), PostgresKernelError> {
    validate_body_reference_evidence(
        active,
        function,
        &expected_body_references(plan),
        "reference count must match signature and plan traversal",
        "references must be ordered signature evidence followed by plan traversal",
    )
}

pub(super) fn validate_identity_selected_reference_evidence(
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    plan: &IdentitySelectedServerPlan,
) -> Result<(), PostgresKernelError> {
    validate_body_reference_evidence(
        active,
        function,
        &expected_identity_selected_body_references(plan),
        "recorded dependencies must match the function signature and query",
        "recorded dependencies must appear in the same order as the function signature and query",
    )
}

pub(super) fn validate_unique_text_selected_reference_evidence(
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    plan: &UniqueTextSelectedServerPlan,
) -> Result<(), PostgresKernelError> {
    validate_body_reference_evidence(
        active,
        function,
        &expected_unique_text_selected_body_references(plan),
        "recorded dependencies must match the unique-Text-selected function signature and query",
        "recorded dependencies must appear in the same order as the unique-Text-selected function signature and query",
    )
}

pub(super) fn validate_distinct_reference_evidence(
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    plan: &DistinctServerPlan,
) -> Result<(), PostgresKernelError> {
    let expected = expected_unordered_body_references(
        plan.scan().object_type,
        plan.projections(),
        plan.selection(),
    );
    validate_function_reference_replay(active, function, &expected)
        .map_err(distinct_reference_error)
}

pub(super) fn distinct_reference_error(mismatch: ReferenceReplayMismatch) -> PostgresKernelError {
    distinct_error(match mismatch {
        ReferenceReplayMismatch::Count => DISTINCT_REFERENCE_COUNT_RULE,
        ReferenceReplayMismatch::Sequence => DISTINCT_REFERENCE_SEQUENCE_RULE,
    })
}

fn validate_body_reference_evidence(
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    expected: &[ExpectedDefinitionReference],
    count_rule: &'static str,
    sequence_rule: &'static str,
) -> Result<(), PostgresKernelError> {
    validate_function_reference_replay(active, function, expected).map_err(|mismatch| {
        let rule = match mismatch {
            ReferenceReplayMismatch::Count => count_rule,
            ReferenceReplayMismatch::Sequence => sequence_rule,
        };
        reference_error(function.id(), rule)
    })
}

pub(super) fn expected_identity_selected_body_references(
    plan: &IdentitySelectedServerPlan,
) -> Vec<ExpectedDefinitionReference> {
    let mut expected = vec![ExpectedDefinitionReference::new(
        DefinitionReferenceKind::QueryObject,
        DefinitionReferenceTarget::ObjectType(plan.scan().object_type),
    )];
    for projection in plan.projections() {
        add_expression_references(&mut expected, plan.scan().object_type, projection);
    }
    expected.push(ExpectedDefinitionReference::new(
        DefinitionReferenceKind::ObjectReference,
        DefinitionReferenceTarget::ObjectType(plan.scan().object_type),
    ));
    let selector = plan.selector();
    expected.push(ExpectedDefinitionReference::new(
        DefinitionReferenceKind::ParameterRead,
        DefinitionReferenceTarget::Parameter {
            owner: selector.owner(),
            parameter: selector.parameter(),
        },
    ));
    expected
}

fn expected_unique_text_selected_body_references(
    plan: &UniqueTextSelectedServerPlan,
) -> Vec<ExpectedDefinitionReference> {
    let mut expected = vec![ExpectedDefinitionReference::new(
        DefinitionReferenceKind::QueryObject,
        DefinitionReferenceTarget::ObjectType(plan.scan().object_type),
    )];
    for projection in plan.projections() {
        add_expression_references(&mut expected, plan.scan().object_type, projection);
    }
    let UniqueTextSelectBindValue::Text {
        field_owner,
        field,
        parameter_owner,
        parameter,
        ..
    } = plan.selector();
    expected.push(ExpectedDefinitionReference::new(
        DefinitionReferenceKind::QueryField,
        DefinitionReferenceTarget::Field {
            owner: *field_owner,
            field: *field,
        },
    ));
    expected.push(ExpectedDefinitionReference::new(
        DefinitionReferenceKind::ParameterRead,
        DefinitionReferenceTarget::Parameter {
            owner: *parameter_owner,
            parameter: *parameter,
        },
    ));
    expected
}

fn expected_body_references(plan: &ServerPlan) -> Vec<ExpectedDefinitionReference> {
    let mut expected = expected_unordered_body_references(
        plan.scan.object_type,
        &plan.projections,
        plan.selection.as_ref(),
    );
    for ordering in &plan.ordering {
        add_expression_references(&mut expected, plan.scan.object_type, &ordering.expression);
    }
    expected
}

pub(super) fn expected_unordered_body_references(
    scan: TypeId,
    projections: &[Expression],
    selection: Option<&Expression>,
) -> Vec<ExpectedDefinitionReference> {
    let mut expected = vec![ExpectedDefinitionReference::new(
        DefinitionReferenceKind::QueryObject,
        DefinitionReferenceTarget::ObjectType(scan),
    )];
    for expression in projections {
        add_expression_references(&mut expected, scan, expression);
    }
    if let Some(selection) = selection {
        add_expression_references(&mut expected, scan, selection);
    }
    expected
}

pub(super) fn add_expression_references(
    expected: &mut Vec<ExpectedDefinitionReference>,
    scan: TypeId,
    expression: &Expression,
) {
    match &expression.kind {
        ExpressionKind::ObjectReference { .. } => {
            expected.push(ExpectedDefinitionReference::new(
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(scan),
            ));
        }
        ExpressionKind::BooleanLiteral { .. } => {}
        ExpressionKind::FieldPath { steps, .. } => {
            for step in steps {
                expected.push(ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::QueryField,
                    DefinitionReferenceTarget::Field {
                        owner: step.owner,
                        field: step.field,
                    },
                ));
            }
        }
        ExpressionKind::Equality { left, right } => {
            add_expression_references(expected, scan, left);
            add_expression_references(expected, scan, right);
        }
    }
}

pub(super) fn result_columns_for_projections(
    function: &FunctionDefinition,
    projections: &[Expression],
) -> Result<Vec<ResultColumn>, PostgresKernelError> {
    let FunctionReturn::Rows(columns) = function.return_type() else {
        return Err(plan_invariant("function return must be ROWS"));
    };
    columns
        .iter()
        .zip(projections)
        .map(|(column, projection)| {
            ResultColumn::new(
                column.name(),
                projection.value_type.resolved_type,
                projection.value_type.nullable,
            )
            .map_err(ServerSelectError::ResultRows)
            .map_err(server_error)
        })
        .collect()
}
