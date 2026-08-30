use super::*;

#[cfg(feature = "test-hooks")]
const RAW_SCALAR_INSERT_SOURCE: &str = "CREATE SCHEMA raw_scalar_insert;\n\
    CREATE TYPE raw_scalar_insert.int_probe AS OBJECT (\n\
      stored INT NOT NULL\n\
    );\n\
    CREATE SERVER FUNCTION raw_scalar_insert.create_int(p_value INT)\n\
    RETURNS ROWS (created REF raw_scalar_insert.int_probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_scalar_insert.int_probe AS made (stored)\n\
    VALUES (p_value) RETURNING REF(made);\n\
    CREATE SERVER FUNCTION raw_scalar_insert.read_ints()\n\
    RETURNS ROWS (stored INT)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT int_probe.stored FROM raw_scalar_insert.int_probe int_probe;\n\
    CREATE TYPE raw_scalar_insert.bigint_probe AS OBJECT (\n\
      stored BIGINT NOT NULL\n\
    );\n\
    CREATE SERVER FUNCTION raw_scalar_insert.create_bigint(p_value BIGINT)\n\
    RETURNS ROWS (created REF raw_scalar_insert.bigint_probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_scalar_insert.bigint_probe AS made (stored)\n\
    VALUES (p_value) RETURNING REF(made);\n\
    CREATE SERVER FUNCTION raw_scalar_insert.read_bigints()\n\
    RETURNS ROWS (stored BIGINT)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT bigint_probe.stored FROM raw_scalar_insert.bigint_probe bigint_probe;\n\
    CREATE TYPE raw_scalar_insert.float_probe AS OBJECT (\n\
      stored FLOAT NOT NULL\n\
    );\n\
    CREATE SERVER FUNCTION raw_scalar_insert.create_float(p_value FLOAT)\n\
    RETURNS ROWS (created REF raw_scalar_insert.float_probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_scalar_insert.float_probe AS made (stored)\n\
    VALUES (p_value) RETURNING REF(made);\n\
    CREATE SERVER FUNCTION raw_scalar_insert.read_floats()\n\
    RETURNS ROWS (stored FLOAT)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT float_probe.stored FROM raw_scalar_insert.float_probe float_probe;\n\
    CREATE TYPE raw_scalar_insert.text_probe AS OBJECT (\n\
      stored TEXT NOT NULL\n\
    );\n\
    CREATE SERVER FUNCTION raw_scalar_insert.create_text(p_value TEXT)\n\
    RETURNS ROWS (created REF raw_scalar_insert.text_probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_scalar_insert.text_probe AS made (stored)\n\
    VALUES (p_value) RETURNING REF(made);\n\
    CREATE SERVER FUNCTION raw_scalar_insert.read_texts()\n\
    RETURNS ROWS (stored TEXT)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT text_probe.stored FROM raw_scalar_insert.text_probe text_probe;\n\
    CREATE SERVER FUNCTION raw_scalar_insert.create_extra(p_used TEXT, p_extra TEXT)\n\
    RETURNS ROWS (created REF raw_scalar_insert.text_probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_scalar_insert.text_probe AS made (stored)\n\
    VALUES (p_used) RETURNING REF(made);\n\
    CREATE TYPE raw_scalar_insert.unused_probe AS OBJECT (\n\
      stored BOOLEAN NOT NULL\n\
    );\n\
    CREATE SERVER FUNCTION raw_scalar_insert.create_unused(p_unused TEXT)\n\
    RETURNS ROWS (created REF raw_scalar_insert.unused_probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_scalar_insert.unused_probe AS made (stored)\n\
    VALUES (TRUE) RETURNING REF(made);\n\
    CREATE SERVER FUNCTION raw_scalar_insert.read_unused()\n\
    RETURNS ROWS (stored BOOLEAN)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT unused_probe.stored FROM raw_scalar_insert.unused_probe unused_probe;\n\
    CREATE TYPE raw_scalar_insert.bytes_probe AS OBJECT (\n\
      stored BYTES NOT NULL\n\
    );\n\
    CREATE SERVER FUNCTION raw_scalar_insert.create_bytes(p_value BYTES)\n\
    RETURNS ROWS (created REF raw_scalar_insert.bytes_probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_scalar_insert.bytes_probe AS made (stored)\n\
    VALUES (p_value) RETURNING REF(made);\n\
    CREATE SERVER FUNCTION raw_scalar_insert.read_bytes()\n\
    RETURNS ROWS (stored BYTES)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT bytes_probe.stored FROM raw_scalar_insert.bytes_probe bytes_probe;\n\
    CREATE TYPE raw_scalar_insert.bool_probe AS OBJECT (\n\
      stored BOOLEAN NOT NULL\n\
    );\n\
    CREATE SERVER FUNCTION raw_scalar_insert.create_bool(p_value BOOLEAN)\n\
    RETURNS ROWS (created REF raw_scalar_insert.bool_probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_scalar_insert.bool_probe AS made (stored)\n\
    VALUES (p_value) RETURNING REF(made);\n\
    CREATE SERVER FUNCTION raw_scalar_insert.read_bools()\n\
    RETURNS ROWS (stored BOOLEAN)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT bool_probe.stored FROM raw_scalar_insert.bool_probe bool_probe;\n";

/// One raw scalar-INSERT fixture: one exact single-parameter INSERT and one
/// public single-column reader per accepted scalar type, plus one Boolean
/// regression pair.
#[cfg(feature = "test-hooks")]
#[derive(Clone, Copy)]
struct RawScalarInsertFixture {
    int_probe: TypeId,
    bigint_probe: TypeId,
    float_probe: TypeId,
    text_probe: TypeId,
    bytes_probe: TypeId,
    bool_probe: TypeId,
    create_int: FunctionId,
    create_bigint: FunctionId,
    create_float: FunctionId,
    create_text: FunctionId,
    create_bytes: FunctionId,
    create_bool: FunctionId,
    create_extra: FunctionId,
    create_unused: FunctionId,
    create_int_parameter: ParameterId,
    create_bigint_parameter: ParameterId,
    create_float_parameter: ParameterId,
    create_text_parameter: ParameterId,
    create_bytes_parameter: ParameterId,
    create_bool_parameter: ParameterId,
    create_extra_used_parameter: ParameterId,
    create_unused_parameter: ParameterId,
    read_ints: FunctionId,
    read_bigints: FunctionId,
    read_floats: FunctionId,
    read_texts: FunctionId,
    read_bytes: FunctionId,
    read_bools: FunctionId,
    read_unused: FunctionId,
}

#[cfg(feature = "test-hooks")]
impl RawScalarInsertFixture {
    fn from_active(active: &ActiveDatabaseRevision) -> TestResult<Self> {
        let object = |name| {
            active
                .catalogue()
                .object_types()
                .iter()
                .find(|object| name_is(object.name().parts(), &["raw_scalar_insert", name]))
                .ok_or_else(|| failure(format!("raw_scalar_insert.{name} type is absent")))
        };
        let function = |name| {
            active
                .catalogue()
                .functions()
                .iter()
                .find(|function| name_is(function.name().parts(), &["raw_scalar_insert", name]))
                .ok_or_else(|| failure(format!("raw_scalar_insert.{name} function is absent")))
        };
        let parameter = |function: &orna_core::catalogue::FunctionDefinition, name| {
            function
                .parameter_by_name(name)
                .map(|parameter| parameter.id())
                .ok_or_else(|| failure(format!("parameter {name} is absent")))
        };
        let int_probe = object("int_probe")?;
        let bigint_probe = object("bigint_probe")?;
        let float_probe = object("float_probe")?;
        let text_probe = object("text_probe")?;
        let bytes_probe = object("bytes_probe")?;
        let bool_probe = object("bool_probe")?;
        let create_int = function("create_int")?;
        let create_bigint = function("create_bigint")?;
        let create_float = function("create_float")?;
        let create_text = function("create_text")?;
        let create_bytes = function("create_bytes")?;
        let create_bool = function("create_bool")?;
        let create_extra = function("create_extra")?;
        let create_unused = function("create_unused")?;
        Ok(Self {
            int_probe: int_probe.id(),
            bigint_probe: bigint_probe.id(),
            float_probe: float_probe.id(),
            text_probe: text_probe.id(),
            bytes_probe: bytes_probe.id(),
            bool_probe: bool_probe.id(),
            create_int: create_int.id(),
            create_bigint: create_bigint.id(),
            create_float: create_float.id(),
            create_text: create_text.id(),
            create_bytes: create_bytes.id(),
            create_bool: create_bool.id(),
            create_extra: create_extra.id(),
            create_unused: create_unused.id(),
            create_int_parameter: parameter(create_int, "p_value")?,
            create_bigint_parameter: parameter(create_bigint, "p_value")?,
            create_float_parameter: parameter(create_float, "p_value")?,
            create_text_parameter: parameter(create_text, "p_value")?,
            create_bytes_parameter: parameter(create_bytes, "p_value")?,
            create_bool_parameter: parameter(create_bool, "p_value")?,
            create_extra_used_parameter: parameter(create_extra, "p_used")?,
            create_unused_parameter: parameter(create_unused, "p_unused")?,
            read_ints: function("read_ints")?.id(),
            read_bigints: function("read_bigints")?.id(),
            read_floats: function("read_floats")?.id(),
            read_texts: function("read_texts")?.id(),
            read_bytes: function("read_bytes")?.id(),
            read_bools: function("read_bools")?.id(),
            read_unused: function("read_unused")?.id(),
        })
    }
}

#[cfg(feature = "test-hooks")]
pub(super) fn raw_scalar_insert_reference(
    result: AuthenticatedRawCallResult,
    target: TypeId,
) -> TestResult<ObjectId> {
    let values = match result {
        AuthenticatedRawCallResult::Server(values) if values.len() == 1 => values,
        other => {
            return Err(failure(format!(
                "raw scalar INSERT must return exactly one Server value, got {other:?}"
            )));
        }
    };
    let RuntimeValue::Reference {
        target: actual_target,
        object,
    } = &values[0]
    else {
        return Err(failure("raw scalar INSERT must return an object reference"));
    };
    require(
        *actual_target == target && *object != ObjectId::from_bytes([0; 16]),
        "raw scalar INSERT reference must name the exact target type and a real row",
    )?;
    Ok(*object)
}

#[cfg(feature = "test-hooks")]
pub(super) async fn read_raw_scalar_values(
    kernel: &PostgresKernel,
    session: &AuthenticatedSession,
    reader: FunctionId,
) -> TestResult<Vec<RuntimeValue>> {
    let read = kernel
        .dispatch_authenticated_raw_call(session, reader)
        .await?;
    match read {
        AuthenticatedRawCallResult::Server(values) => Ok(values),
        other => Err(failure(format!(
            "raw scalar SELECT must return Server values, got {other:?}"
        ))),
    }
}

#[cfg(feature = "test-hooks")]
pub(super) fn require_exact_scalar_read(
    values: &[RuntimeValue],
    expected: &RuntimeValue,
    label: &str,
) -> TestResult<()> {
    require(
        values == std::slice::from_ref(expected),
        format!("{label} reader must return exactly the stored value"),
    )
}

#[cfg(feature = "test-hooks")]
pub(super) fn require_raw_scalar_target_unavailable(
    error: &PostgresKernelError,
    function: FunctionId,
    rule: &'static str,
) -> TestResult<()> {
    require(
        matches!(
            error,
            PostgresKernelError::RawCallTargetUnavailable {
                function: actual,
                rule: actual_rule,
            } if *actual == function && *actual_rule == rule
        ),
        "raw scalar target unavailable error lost its exact function or rule",
    )
}

/// One authenticated raw scalar-INSERT journey across denial, grant,
/// wrong-binding rejection, Text U+0000 rejection, exact stored values,
/// Boolean regression, and public recovery.
///
/// The test installs the exact checked-in scalar-INSERT source with one
/// single-parameter INSERT and one reader per accepted scalar type. It grants
/// only the seven readers, then proves a wrong-parameter Integer call and a
/// U+0000 Text call are each denied before their grants with zero rows.
/// After granting the eight INSERT targets, a wrong parameter id, a wrong
/// scalar type, an extra declared parameter, and an unused sole scalar
/// parameter each close as the exact `raw SERVER INSERT argument target is
/// unavailable` rule, roll back their savepoints, and retain their allowed
/// audits. An allowed Integer-bearing raw SELECT closes as the unsupported
/// raw target rule. Text U+0000 returns the same unavailable raw target
/// after an allowed audit, creates no row, and never reaches the driver bind.
/// Each exact scalar then binds its exact `ParameterId` and direct assignment,
/// returns a distinct nonzero typed reference, and the public reader returns
/// the exact stored value and byte pattern. A Boolean INSERT proves the
/// accepted Boolean shape remains unchanged beside the five new scalars.
/// Recovery proves the active pair is unchanged and the grant set is exactly
/// the fifteen fixed-service grants. Rows are asserted only through the
/// public raw readers.
///
/// The authenticated Reference INSERT journey stays covered by the separate
/// `authenticated_raw_reference_insert_is_denied_then_granted_transactional_and_unique`
/// test; this test does not duplicate its reference setup.
#[cfg(feature = "test-hooks")]
#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn authenticated_raw_scalar_insert_binds_exact_parameters_and_stores_exact_values() -> TestResult<()>
{
    run_large_stack_live_test(
        "authenticated-raw-scalar-insert-live",
        authenticated_raw_scalar_insert_binds_exact_parameters_and_stores_exact_values_inner,
    )
}

#[cfg(feature = "test-hooks")]
async fn authenticated_raw_scalar_insert_binds_exact_parameters_and_stores_exact_values_inner()
-> TestResult<()> {
    with_test_database(|database| async move {
        let kernel: PostgresKernel = database.connection_string().parse()?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&empty)?;
        let standard = kernel.apply_standard_upgrade(&upgrade).await?;
        let applied = kernel
            .apply(&standard_application_candidate(
                RAW_SCALAR_INSERT_SOURCE,
                &standard,
                &upgrade,
            )?)
            .await?;
        let pair = applied.pair();
        let fixture = RawScalarInsertFixture::from_active(&applied)?;
        let mut wrong_parameter_bytes = fixture.create_int_parameter.to_bytes();
        wrong_parameter_bytes[0] ^= 0x01;
        let wrong_parameter = ParameterId::from_bytes(wrong_parameter_bytes);
        require(
            wrong_parameter != fixture.create_int_parameter,
            "the deliberately wrong parameter must differ from the declared parameter",
        )?;
        let u0000_text = RuntimeValue::Text(String::from("a\u{0}b"));
        kernel.install_catalogue_health_service(SERVICE_UID).await?;
        let session = kernel.authenticate_local_peer(SERVICE_UID).await?;

        // Grant only the seven readers; every INSERT target stays
        // unauthorised for the denial proof.
        for reader in [
            fixture.read_ints,
            fixture.read_bigints,
            fixture.read_floats,
            fixture.read_texts,
            fixture.read_bytes,
            fixture.read_bools,
            fixture.read_unused,
        ] {
            kernel
                .grant_catalogue_health_service_execute(pair, reader)
                .await?;
        }

        // Denial wins over every parameter, type, and U+0000 fact: the
        // wrong-parameter Integer call and the U+0000 Text call are denied
        // before their grants, and no row exists.
        let denied_wrong_parameter = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_int,
                &[FunctionArgument::new(
                    wrong_parameter,
                    RuntimeValue::Integer(7),
                )?],
            )
            .await
            .expect_err("an ungranted raw Integer INSERT must be denied");
        require(
            matches!(
                denied_wrong_parameter,
                PostgresKernelError::RawExecuteDenied {
                    pair: denied_pair,
                    function: denied_function,
                    reason: ExecuteDenial::MissingExecuteGrant,
                } if denied_pair == pair && denied_function == fixture.create_int
            ),
            "pre-grant wrong-parameter raw Integer INSERT returned the wrong typed denial",
        )?;
        let denied_u0000 = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_text,
                &[FunctionArgument::new(
                    fixture.create_text_parameter,
                    u0000_text.clone(),
                )?],
            )
            .await
            .expect_err("an ungranted U+0000 raw Text INSERT must be denied");
        require(
            matches!(
                denied_u0000,
                PostgresKernelError::RawExecuteDenied {
                    pair: denied_pair,
                    function: denied_function,
                    reason: ExecuteDenial::MissingExecuteGrant,
                } if denied_pair == pair && denied_function == fixture.create_text
            ),
            "pre-grant U+0000 raw Text INSERT returned the wrong typed denial",
        )?;
        let zero_ints = read_raw_scalar_values(&kernel, &session, fixture.read_ints).await?;
        require(
            zero_ints.is_empty(),
            "the denied raw Integer INSERT must not create any row",
        )?;
        let zero_texts = read_raw_scalar_values(&kernel, &session, fixture.read_texts).await?;
        require(
            zero_texts.is_empty(),
            "the denied U+0000 raw Text INSERT must not create any row",
        )?;

        // Grant the eight INSERT targets.
        for create in [
            fixture.create_int,
            fixture.create_bigint,
            fixture.create_float,
            fixture.create_text,
            fixture.create_bytes,
            fixture.create_bool,
            fixture.create_extra,
            fixture.create_unused,
        ] {
            kernel
                .grant_catalogue_health_service_execute(pair, create)
                .await?;
        }

        // A wrong parameter id closes as the exact unavailable raw target,
        // rolls back its savepoint, and retains its allowed audit.
        let wrong_parameter_unavailable = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_int,
                &[FunctionArgument::new(
                    wrong_parameter,
                    RuntimeValue::Integer(7),
                )?],
            )
            .await
            .expect_err("a wrong parameter id must make the raw Integer INSERT unavailable");
        require_raw_scalar_target_unavailable(
            &wrong_parameter_unavailable,
            fixture.create_int,
            "raw SERVER INSERT argument target is unavailable",
        )?;
        let after_wrong_parameter =
            read_raw_scalar_values(&kernel, &session, fixture.read_ints).await?;
        require(
            after_wrong_parameter.is_empty(),
            "a wrong parameter id must not create any row",
        )?;

        // A wrong scalar type closes with the same exact rule.
        let wrong_type_unavailable = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_text,
                &[FunctionArgument::new(
                    fixture.create_text_parameter,
                    RuntimeValue::Integer(7),
                )?],
            )
            .await
            .expect_err("an Integer argument must make the raw Text INSERT unavailable");
        require_raw_scalar_target_unavailable(
            &wrong_type_unavailable,
            fixture.create_text,
            "raw SERVER INSERT argument target is unavailable",
        )?;
        let after_wrong_type =
            read_raw_scalar_values(&kernel, &session, fixture.read_texts).await?;
        require(
            after_wrong_type.is_empty(),
            "a wrong scalar type must not create any row",
        )?;

        // An allowed scalar-bearing non-INSERT SERVER target closes as the
        // unsupported raw target rule.
        let unsupported_select = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.read_ints,
                &[FunctionArgument::new(
                    fixture.create_int_parameter,
                    RuntimeValue::Integer(7),
                )?],
            )
            .await
            .expect_err("an Integer argument must reject the granted raw SELECT");
        require_raw_scalar_target_unavailable(
            &unsupported_select,
            fixture.read_ints,
            "raw call arguments require a supported active SERVER mutation target",
        )?;
        let after_unsupported =
            read_raw_scalar_values(&kernel, &session, fixture.read_ints).await?;
        require(
            after_unsupported.is_empty(),
            "an unsupported scalar target must not create any row",
        )?;

        // Text U+0000 is an authorised target failure: it rolls back the raw
        // INSERT savepoint, creates no row, and retains its allowed audit.
        let u0000_unavailable = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_text,
                &[FunctionArgument::new(
                    fixture.create_text_parameter,
                    u0000_text,
                )?],
            )
            .await
            .expect_err("Text U+0000 must make the raw Text INSERT unavailable");
        require_raw_scalar_target_unavailable(
            &u0000_unavailable,
            fixture.create_text,
            "raw SERVER INSERT argument target is unavailable",
        )?;
        let after_u0000 = read_raw_scalar_values(&kernel, &session, fixture.read_texts).await?;
        require(
            after_u0000.is_empty(),
            "Text U+0000 must not create any row",
        )?;

        // An INSERT target that declares a second parameter closes with the
        // same exact rule even though the supplied argument names the one
        // parameter the plan reads.
        let extra_parameter_unavailable = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_extra,
                &[FunctionArgument::new(
                    fixture.create_extra_used_parameter,
                    RuntimeValue::Text(String::from("extra")),
                )?],
            )
            .await
            .expect_err("an extra declared parameter must make the raw Text INSERT unavailable");
        require_raw_scalar_target_unavailable(
            &extra_parameter_unavailable,
            fixture.create_extra,
            "raw SERVER INSERT argument target is unavailable",
        )?;
        let after_extra = read_raw_scalar_values(&kernel, &session, fixture.read_texts).await?;
        require(
            after_extra.is_empty(),
            "an extra declared parameter must not create any row",
        )?;

        // An INSERT target whose sole scalar parameter is never read by a
        // direct assignment closes with the same exact rule.
        let unused_parameter_unavailable = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_unused,
                &[FunctionArgument::new(
                    fixture.create_unused_parameter,
                    RuntimeValue::Text(String::from("unused")),
                )?],
            )
            .await
            .expect_err("an unused sole parameter must make the raw Text INSERT unavailable");
        require_raw_scalar_target_unavailable(
            &unused_parameter_unavailable,
            fixture.create_unused,
            "raw SERVER INSERT argument target is unavailable",
        )?;
        let after_unused = read_raw_scalar_values(&kernel, &session, fixture.read_unused).await?;
        require(
            after_unused.is_empty(),
            "an unused sole parameter must not create any row",
        )?;

        // Each exact scalar binds its exact ParameterId through a direct
        // assignment and stores its exact value; every INSERT returns a
        // distinct nonzero reference to its exact target type.
        let mut identities = BTreeSet::new();
        let inserted_int = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_int,
                &[FunctionArgument::new(
                    fixture.create_int_parameter,
                    RuntimeValue::Integer(i32::MIN),
                )?],
            )
            .await?;
        let int_object = raw_scalar_insert_reference(inserted_int, fixture.int_probe)?;
        require(
            identities.insert(int_object),
            "raw scalar INSERTs must allocate distinct object identities",
        )?;
        let ints = read_raw_scalar_values(&kernel, &session, fixture.read_ints).await?;
        require_exact_scalar_read(&ints, &RuntimeValue::Integer(i32::MIN), "INT")?;

        let inserted_bigint = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_bigint,
                &[FunctionArgument::new(
                    fixture.create_bigint_parameter,
                    RuntimeValue::BigInt(i64::MAX),
                )?],
            )
            .await?;
        let bigint_object = raw_scalar_insert_reference(inserted_bigint, fixture.bigint_probe)?;
        require(
            identities.insert(bigint_object),
            "raw scalar INSERTs must allocate distinct object identities",
        )?;
        let bigints = read_raw_scalar_values(&kernel, &session, fixture.read_bigints).await?;
        require_exact_scalar_read(&bigints, &RuntimeValue::BigInt(i64::MAX), "BIGINT")?;

        let inserted_float = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_float,
                &[FunctionArgument::new(
                    fixture.create_float_parameter,
                    RuntimeValue::Float(RuntimeFloat::new(0.1)?),
                )?],
            )
            .await?;
        let float_object = raw_scalar_insert_reference(inserted_float, fixture.float_probe)?;
        require(
            identities.insert(float_object),
            "raw scalar INSERTs must allocate distinct object identities",
        )?;
        let floats = read_raw_scalar_values(&kernel, &session, fixture.read_floats).await?;
        require_exact_scalar_read(
            &floats,
            &RuntimeValue::Float(RuntimeFloat::new(0.1)?),
            "FLOAT",
        )?;
        let RuntimeValue::Float(stored_float) = &floats[0] else {
            return Err(failure("raw FLOAT reader must return a Float value"));
        };
        require(
            stored_float.value().to_bits() == 0.1_f64.to_bits(),
            "FLOAT reader must preserve the exact canonical bit pattern",
        )?;

        let inserted_text = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_text,
                &[FunctionArgument::new(
                    fixture.create_text_parameter,
                    RuntimeValue::Text(String::from("caf\u{e9} e\u{301}\n\t\u{65e5}\u{672c}")),
                )?],
            )
            .await?;
        let text_object = raw_scalar_insert_reference(inserted_text, fixture.text_probe)?;
        require(
            identities.insert(text_object),
            "raw scalar INSERTs must allocate distinct object identities",
        )?;
        let texts = read_raw_scalar_values(&kernel, &session, fixture.read_texts).await?;
        require_exact_scalar_read(
            &texts,
            &RuntimeValue::Text(String::from("caf\u{e9} e\u{301}\n\t\u{65e5}\u{672c}")),
            "TEXT",
        )?;

        let inserted_bytes = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_bytes,
                &[FunctionArgument::new(
                    fixture.create_bytes_parameter,
                    RuntimeValue::Bytes(vec![0x00, 0xff, 0x7f, 0x00, 0x01]),
                )?],
            )
            .await?;
        let bytes_object = raw_scalar_insert_reference(inserted_bytes, fixture.bytes_probe)?;
        require(
            identities.insert(bytes_object),
            "raw scalar INSERTs must allocate distinct object identities",
        )?;
        let bytes = read_raw_scalar_values(&kernel, &session, fixture.read_bytes).await?;
        require_exact_scalar_read(
            &bytes,
            &RuntimeValue::Bytes(vec![0x00, 0xff, 0x7f, 0x00, 0x01]),
            "BYTES",
        )?;

        // The Boolean shape remains accepted beside the five new scalars.
        // The Reference INSERT regression is not duplicated here: it stays
        // covered by the dedicated reference-INSERT test above.
        let inserted_bool = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_bool,
                &[FunctionArgument::new(
                    fixture.create_bool_parameter,
                    RuntimeValue::Boolean(true),
                )?],
            )
            .await?;
        let bool_object = raw_scalar_insert_reference(inserted_bool, fixture.bool_probe)?;
        require(
            identities.insert(bool_object),
            "raw scalar INSERTs must allocate distinct object identities",
        )?;
        let bools = read_raw_scalar_values(&kernel, &session, fixture.read_bools).await?;
        require_exact_scalar_read(&bools, &RuntimeValue::Boolean(true), "BOOLEAN")?;
        require(
            identities.len() == 6,
            "the six successful raw scalar INSERTs must use six distinct identities",
        )?;

        // One authentication decision, then one execute decision per
        // dispatched call in dispatch order. The only denied execute
        // decisions are the two pre-grant calls; every granted call,
        // including every rejected target, retained exactly one allowed
        // execute decision at its dispatch position.
        let audits = kernel.recover_security_audit_events().await?;
        let actual: Vec<(SecurityAuditKind, SecurityAuditOutcome, Option<FunctionId>)> = audits
            .iter()
            .map(|event| {
                let decision = event.decision();
                (
                    decision.kind(),
                    decision.outcome(),
                    decision.target().map(InvocationTarget::function),
                )
            })
            .collect();
        let expected: Vec<(SecurityAuditKind, SecurityAuditOutcome, Option<FunctionId>)> = vec![
            (
                SecurityAuditKind::Authentication,
                SecurityAuditOutcome::Allowed,
                None,
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Denied,
                Some(fixture.create_int),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Denied,
                Some(fixture.create_text),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.read_ints),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.read_texts),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.create_int),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.read_ints),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.create_text),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.read_texts),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.read_ints),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.read_ints),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.create_text),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.read_texts),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.create_extra),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.read_texts),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.create_unused),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.read_unused),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.create_int),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.read_ints),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.create_bigint),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.read_bigints),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.create_float),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.read_floats),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.create_text),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.read_texts),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.create_bytes),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.read_bytes),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.create_bool),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.read_bools),
            ),
        ];
        require(
            actual == expected,
            "raw scalar INSERT audit chain differs from the dispatch order",
        )?;

        // Public recovery proves the exact fixed-service grant set.
        let mut grants = kernel
            .recover_security_snapshot()
            .await?
            .execute_grants()
            .collect::<Vec<_>>();
        grants.sort();
        let mut expected = vec![
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.create_int),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.create_bigint),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.create_float),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.create_text),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.create_bytes),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.create_bool),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.create_extra),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.create_unused),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.read_ints),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.read_bigints),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.read_floats),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.read_texts),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.read_bytes),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.read_bools),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.read_unused),
        ];
        expected.sort();
        require(
            grants == expected,
            "recovered grants must contain exactly the fifteen fixed-service grants",
        )?;

        // The active revision pair is unchanged throughout.
        let recovered = kernel.recover().await?;
        require(
            recovered.pair() == pair,
            "raw scalar INSERTs must not change the active revision pair",
        )?;

        require_no_session_leaks(&database).await
    })
    .await
}
