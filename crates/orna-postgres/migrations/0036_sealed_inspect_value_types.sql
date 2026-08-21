-- Admit the transient sealed Inspector carriers only in function parameter and
-- function result value slots. They have no standard-library revision pin and
-- must never become durable catalogue fields or ROWS return columns.

ALTER TABLE _orna_kernel.catalogue_function_parameters
    DROP CONSTRAINT catalogue_function_parameters_check,
    ADD CONSTRAINT catalogue_function_parameters_check CHECK (
        (type_kind = 'scalar'
            AND scalar_type IS NOT NULL
            AND target_type_id IS NULL
            AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NULL
            AND record_type_id IS NULL)
        OR (type_kind IN ('named', 'reference')
            AND scalar_type IS NULL
            AND target_type_id IS NOT NULL
            AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NULL
            AND record_type_id IS NULL)
        OR (type_kind = 'value'
            AND scalar_type IS NULL
            AND target_type_id IS NULL
            AND value_type_id IS NOT NULL
            AND (
                (value_standard_library_revision_id IS NOT NULL
                 AND value_type_id NOT IN (
                    decode('000000000000000000000000000000f3', 'hex'),
                    decode('000000000000000000000000000000f4', 'hex'),
                    decode('000000000000000000000000000000f5', 'hex'),
                    decode('000000000000000000000000000000f6', 'hex'),
                    decode('000000000000000000000000000000f8', 'hex'),
                    decode('000000000000000000000000000000f9', 'hex'),
                    decode('000000000000000000000000000000fa', 'hex'),
                    decode('000000000000000000000000000000fb', 'hex'),
                    decode('000000000000000000000000000000fc', 'hex'),
                    decode('000000000000000000000000000000fd', 'hex'),
                    decode('000000000000000000000000000000fe', 'hex'),
                    decode('000000000000000000000000000000ff', 'hex')
                )
                )
                OR (value_standard_library_revision_id IS NULL
                 AND value_type_id IN (
                    decode('000000000000000000000000000000f3', 'hex'),
                    decode('000000000000000000000000000000f4', 'hex'),
                    decode('000000000000000000000000000000f5', 'hex'),
                    decode('000000000000000000000000000000f6', 'hex'),
                    decode('000000000000000000000000000000f8', 'hex'),
                    decode('000000000000000000000000000000f9', 'hex'),
                    decode('000000000000000000000000000000fa', 'hex'),
                    decode('000000000000000000000000000000fb', 'hex'),
                    decode('000000000000000000000000000000fc', 'hex'),
                    decode('000000000000000000000000000000fd', 'hex'),
                    decode('000000000000000000000000000000fe', 'hex'),
                    decode('000000000000000000000000000000ff', 'hex')
                )
                )
            )
            AND enum_type_id IS NULL
            AND record_type_id IS NULL)
        OR (type_kind = 'enum'
            AND scalar_type IS NULL
            AND target_type_id IS NULL
            AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NOT NULL
            AND record_type_id IS NULL)
        OR (type_kind = 'record'
            AND scalar_type IS NULL
            AND target_type_id IS NULL
            AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NULL
            AND record_type_id IS NOT NULL)
    ),
    ADD CONSTRAINT catalogue_function_parameters_value_pin_check CHECK (
        type_kind <> 'value'
        OR (
            value_standard_library_revision_id IS NOT NULL
            AND value_type_id IS NOT NULL
            AND value_type_id NOT IN (
                decode('000000000000000000000000000000f3', 'hex'),
                decode('000000000000000000000000000000f4', 'hex'),
                decode('000000000000000000000000000000f5', 'hex'),
                decode('000000000000000000000000000000f6', 'hex'),
                decode('000000000000000000000000000000f8', 'hex'),
                decode('000000000000000000000000000000f9', 'hex'),
                decode('000000000000000000000000000000fa', 'hex'),
                decode('000000000000000000000000000000fb', 'hex'),
                decode('000000000000000000000000000000fc', 'hex'),
                decode('000000000000000000000000000000fd', 'hex'),
                decode('000000000000000000000000000000fe', 'hex'),
                decode('000000000000000000000000000000ff', 'hex')
            )
        )
        OR (value_standard_library_revision_id IS NULL
            AND value_type_id IS NOT NULL
            AND value_type_id IN (
                decode('000000000000000000000000000000f3', 'hex'),
                decode('000000000000000000000000000000f4', 'hex'),
                decode('000000000000000000000000000000f5', 'hex'),
                decode('000000000000000000000000000000f6', 'hex'),
                decode('000000000000000000000000000000f8', 'hex'),
                decode('000000000000000000000000000000f9', 'hex'),
                decode('000000000000000000000000000000fa', 'hex'),
                decode('000000000000000000000000000000fb', 'hex'),
                decode('000000000000000000000000000000fc', 'hex'),
                decode('000000000000000000000000000000fd', 'hex'),
                decode('000000000000000000000000000000fe', 'hex'),
                decode('000000000000000000000000000000ff', 'hex')
            )
        )
    );

ALTER TABLE _orna_kernel.catalogue_functions
    DROP CONSTRAINT catalogue_functions_check1,
    ADD CONSTRAINT catalogue_functions_check1 CHECK (
        (return_shape = 'rows'
            AND return_type_kind IS NULL
            AND return_scalar_type IS NULL
            AND return_target_type_id IS NULL
            AND return_value_type_id IS NULL
            AND return_standard_library_revision_id IS NULL
            AND return_enum_type_id IS NULL
            AND return_record_type_id IS NULL)
        OR (return_shape = 'single' AND (
            (return_type_kind = 'scalar'
                AND return_scalar_type IS NOT NULL
                AND return_target_type_id IS NULL
                AND return_value_type_id IS NULL
                AND return_standard_library_revision_id IS NULL
                AND return_enum_type_id IS NULL
                AND return_record_type_id IS NULL)
            OR (return_type_kind IN ('named', 'reference')
                AND return_scalar_type IS NULL
                AND return_target_type_id IS NOT NULL
                AND return_value_type_id IS NULL
                AND return_standard_library_revision_id IS NULL
                AND return_enum_type_id IS NULL
                AND return_record_type_id IS NULL)
            OR (return_type_kind = 'value'
                AND return_scalar_type IS NULL
                AND return_target_type_id IS NULL
                AND return_value_type_id IS NOT NULL
                AND (
                    (return_standard_library_revision_id IS NOT NULL
                     AND return_value_type_id NOT IN (
                        decode('000000000000000000000000000000f3', 'hex'),
                        decode('000000000000000000000000000000f4', 'hex'),
                        decode('000000000000000000000000000000f5', 'hex'),
                        decode('000000000000000000000000000000f6', 'hex'),
                        decode('000000000000000000000000000000f8', 'hex'),
                        decode('000000000000000000000000000000f9', 'hex'),
                        decode('000000000000000000000000000000fa', 'hex'),
                        decode('000000000000000000000000000000fb', 'hex'),
                        decode('000000000000000000000000000000fc', 'hex'),
                        decode('000000000000000000000000000000fd', 'hex'),
                        decode('000000000000000000000000000000fe', 'hex'),
                        decode('000000000000000000000000000000ff', 'hex')
                    )
                    )
                    OR (return_standard_library_revision_id IS NULL
                     AND return_value_type_id IN (
                        decode('000000000000000000000000000000f3', 'hex'),
                        decode('000000000000000000000000000000f4', 'hex'),
                        decode('000000000000000000000000000000f5', 'hex'),
                        decode('000000000000000000000000000000f6', 'hex'),
                        decode('000000000000000000000000000000f8', 'hex'),
                        decode('000000000000000000000000000000f9', 'hex'),
                        decode('000000000000000000000000000000fa', 'hex'),
                        decode('000000000000000000000000000000fb', 'hex'),
                        decode('000000000000000000000000000000fc', 'hex'),
                        decode('000000000000000000000000000000fd', 'hex'),
                        decode('000000000000000000000000000000fe', 'hex'),
                        decode('000000000000000000000000000000ff', 'hex')
                    )
                    )
                )
                AND return_enum_type_id IS NULL
                AND return_record_type_id IS NULL)
            OR (return_type_kind = 'enum'
                AND return_scalar_type IS NULL
                AND return_target_type_id IS NULL
                AND return_value_type_id IS NULL
                AND return_standard_library_revision_id IS NULL
                AND return_enum_type_id IS NOT NULL
                AND return_record_type_id IS NULL)
            OR (return_type_kind = 'record'
                AND return_scalar_type IS NULL
                AND return_target_type_id IS NULL
                AND return_value_type_id IS NULL
                AND return_standard_library_revision_id IS NULL
                AND return_enum_type_id IS NULL
                AND return_record_type_id IS NOT NULL)
        ))
        OR (return_shape = 'stream' AND (
            (return_type_kind = 'scalar'
                AND return_scalar_type IS NOT NULL
                AND return_target_type_id IS NULL
                AND return_value_type_id IS NULL
                AND return_standard_library_revision_id IS NULL
                AND return_enum_type_id IS NULL
                AND return_record_type_id IS NULL)
            OR (return_type_kind IN ('named', 'reference')
                AND return_scalar_type IS NULL
                AND return_target_type_id IS NOT NULL
                AND return_value_type_id IS NULL
                AND return_standard_library_revision_id IS NULL
                AND return_enum_type_id IS NULL
                AND return_record_type_id IS NULL)
            OR (return_type_kind = 'value'
                AND return_scalar_type IS NULL
                AND return_target_type_id IS NULL
                AND return_value_type_id IS NOT NULL
                AND (
                    (return_standard_library_revision_id IS NOT NULL
                     AND return_value_type_id NOT IN (
                        decode('000000000000000000000000000000f3', 'hex'),
                        decode('000000000000000000000000000000f4', 'hex'),
                        decode('000000000000000000000000000000f5', 'hex'),
                        decode('000000000000000000000000000000f6', 'hex'),
                        decode('000000000000000000000000000000f8', 'hex'),
                        decode('000000000000000000000000000000f9', 'hex'),
                        decode('000000000000000000000000000000fa', 'hex'),
                        decode('000000000000000000000000000000fb', 'hex'),
                        decode('000000000000000000000000000000fc', 'hex'),
                        decode('000000000000000000000000000000fd', 'hex'),
                        decode('000000000000000000000000000000fe', 'hex'),
                        decode('000000000000000000000000000000ff', 'hex')
                    )
                    )
                    OR (return_standard_library_revision_id IS NULL
                     AND return_value_type_id IN (
                        decode('000000000000000000000000000000f3', 'hex'),
                        decode('000000000000000000000000000000f4', 'hex'),
                        decode('000000000000000000000000000000f5', 'hex'),
                        decode('000000000000000000000000000000f6', 'hex'),
                        decode('000000000000000000000000000000f8', 'hex'),
                        decode('000000000000000000000000000000f9', 'hex'),
                        decode('000000000000000000000000000000fa', 'hex'),
                        decode('000000000000000000000000000000fb', 'hex'),
                        decode('000000000000000000000000000000fc', 'hex'),
                        decode('000000000000000000000000000000fd', 'hex'),
                        decode('000000000000000000000000000000fe', 'hex'),
                        decode('000000000000000000000000000000ff', 'hex')
                    )
                    )
                )
                AND return_enum_type_id IS NULL
                AND return_record_type_id IS NULL)
            OR (return_type_kind = 'enum'
                AND return_scalar_type IS NULL
                AND return_target_type_id IS NULL
                AND return_value_type_id IS NULL
                AND return_standard_library_revision_id IS NULL
                AND return_enum_type_id IS NOT NULL
                AND return_record_type_id IS NULL)
            OR (return_type_kind = 'record'
                AND return_scalar_type IS NULL
                AND return_target_type_id IS NULL
                AND return_value_type_id IS NULL
                AND return_standard_library_revision_id IS NULL
                AND return_enum_type_id IS NULL
                AND return_record_type_id IS NOT NULL)
        ))
    ),
    ADD CONSTRAINT catalogue_functions_return_value_pin_check CHECK (
        return_type_kind IS DISTINCT FROM 'value'
        OR (
            return_standard_library_revision_id IS NOT NULL
            AND return_value_type_id IS NOT NULL
            AND return_value_type_id NOT IN (
                decode('000000000000000000000000000000f3', 'hex'),
                decode('000000000000000000000000000000f4', 'hex'),
                decode('000000000000000000000000000000f5', 'hex'),
                decode('000000000000000000000000000000f6', 'hex'),
                decode('000000000000000000000000000000f8', 'hex'),
                decode('000000000000000000000000000000f9', 'hex'),
                decode('000000000000000000000000000000fa', 'hex'),
                decode('000000000000000000000000000000fb', 'hex'),
                decode('000000000000000000000000000000fc', 'hex'),
                decode('000000000000000000000000000000fd', 'hex'),
                decode('000000000000000000000000000000fe', 'hex'),
                decode('000000000000000000000000000000ff', 'hex')
            )
        )
        OR (return_standard_library_revision_id IS NULL
            AND return_value_type_id IS NOT NULL
            AND return_value_type_id IN (
                decode('000000000000000000000000000000f3', 'hex'),
                decode('000000000000000000000000000000f4', 'hex'),
                decode('000000000000000000000000000000f5', 'hex'),
                decode('000000000000000000000000000000f6', 'hex'),
                decode('000000000000000000000000000000f8', 'hex'),
                decode('000000000000000000000000000000f9', 'hex'),
                decode('000000000000000000000000000000fa', 'hex'),
                decode('000000000000000000000000000000fb', 'hex'),
                decode('000000000000000000000000000000fc', 'hex'),
                decode('000000000000000000000000000000fd', 'hex'),
                decode('000000000000000000000000000000fe', 'hex'),
                decode('000000000000000000000000000000ff', 'hex')
            )
        )
    );

ALTER TABLE _orna_kernel.definition_references
    DROP CONSTRAINT definition_references_target_std_lib_rev_shape_check,
    ADD CONSTRAINT definition_references_target_std_lib_rev_shape_check CHECK (
        (
            target_kind = 'value_type'
            AND (
                (target_standard_library_revision_id IS NOT NULL
                 AND target_definition_id NOT IN (
                    decode('000000000000000000000000000000f3', 'hex'),
                    decode('000000000000000000000000000000f4', 'hex'),
                    decode('000000000000000000000000000000f5', 'hex'),
                    decode('000000000000000000000000000000f6', 'hex'),
                    decode('000000000000000000000000000000f8', 'hex'),
                    decode('000000000000000000000000000000f9', 'hex'),
                    decode('000000000000000000000000000000fa', 'hex'),
                    decode('000000000000000000000000000000fb', 'hex'),
                    decode('000000000000000000000000000000fc', 'hex'),
                    decode('000000000000000000000000000000fd', 'hex'),
                    decode('000000000000000000000000000000fe', 'hex'),
                    decode('000000000000000000000000000000ff', 'hex')
                )
                )
                OR (target_standard_library_revision_id IS NULL
                 AND target_definition_id IN (
                    decode('000000000000000000000000000000f3', 'hex'),
                    decode('000000000000000000000000000000f4', 'hex'),
                    decode('000000000000000000000000000000f5', 'hex'),
                    decode('000000000000000000000000000000f6', 'hex'),
                    decode('000000000000000000000000000000f8', 'hex'),
                    decode('000000000000000000000000000000f9', 'hex'),
                    decode('000000000000000000000000000000fa', 'hex'),
                    decode('000000000000000000000000000000fb', 'hex'),
                    decode('000000000000000000000000000000fc', 'hex'),
                    decode('000000000000000000000000000000fd', 'hex'),
                    decode('000000000000000000000000000000fe', 'hex'),
                    decode('000000000000000000000000000000ff', 'hex')
                )
                )
            )
        )
        OR (
            target_kind <> 'value_type'
            AND target_standard_library_revision_id IS NULL
        )
    );


-- A sealed Inspector carrier is a transient system value, not an application
-- object row. Keep ordinary REF targets under the original deferred integrity
-- contract while admitting only the closed Inspector identity set.
ALTER TABLE _orna_kernel.catalogue_function_parameters
    DROP CONSTRAINT catalogue_function_parameters_catalogue_revision_id_target_fkey;

CREATE FUNCTION _orna_kernel.validate_catalogue_function_parameter_target()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, _orna_kernel
AS $param_target$
DECLARE
    current_type_kind text;
    current_target_type_id bytea;
BEGIN
    -- Deferred trigger events can outlive an INSERT/UPDATE that was later
    -- deleted or reverted. Validate only the current final row for this key.
    SELECT parameter.type_kind, parameter.target_type_id
    INTO current_type_kind, current_target_type_id
    FROM _orna_kernel.catalogue_function_parameters AS parameter
    WHERE parameter.catalogue_revision_id = NEW.catalogue_revision_id
      AND parameter.function_id = NEW.function_id
      AND parameter.parameter_id = NEW.parameter_id
    FOR KEY SHARE;

    IF NOT FOUND OR current_type_kind NOT IN ('named', 'reference') THEN
        RETURN NEW;
    END IF;

    IF current_type_kind = 'reference'
       AND current_target_type_id = decode('000000000000000000000000000000f3', 'hex') THEN
        RETURN NEW;
    END IF;

    PERFORM 1
    FROM _orna_kernel.catalogue_object_types AS target
    WHERE target.catalogue_revision_id = NEW.catalogue_revision_id
      AND target.type_id = current_target_type_id
    FOR KEY SHARE;

    IF NOT FOUND THEN
        RAISE EXCEPTION
            'catalogue function parameter target does not exist for catalogue revision'
            USING ERRCODE = 'foreign_key_violation';
    END IF;

    RETURN NEW;
END;
$param_target$;

REVOKE ALL ON FUNCTION _orna_kernel.validate_catalogue_function_parameter_target() FROM PUBLIC;

CREATE CONSTRAINT TRIGGER catalogue_function_parameters_catalogue_revision_id_target_fkey
    AFTER INSERT OR UPDATE OF type_kind, catalogue_revision_id, target_type_id
    ON _orna_kernel.catalogue_function_parameters
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION _orna_kernel.validate_catalogue_function_parameter_target();


-- Sealed Inspector carriers may also be REF handles in function signatures.
-- Function return columns remain ordinary durable ROWS fields and retain their
-- original foreign key unchanged.
ALTER TABLE _orna_kernel.catalogue_functions
    DROP CONSTRAINT catalogue_functions_catalogue_revision_id_return_target_ty_fkey;

CREATE FUNCTION _orna_kernel.validate_catalogue_function_return_target()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, _orna_kernel
AS $return_target$
DECLARE
    current_return_type_kind text;
    current_return_target_type_id bytea;
BEGIN
    -- Validate the current final function row, not a stale deferred event.
    SELECT function_row.return_type_kind, function_row.return_target_type_id
    INTO current_return_type_kind, current_return_target_type_id
    FROM _orna_kernel.catalogue_functions AS function_row
    WHERE function_row.catalogue_revision_id = NEW.catalogue_revision_id
      AND function_row.function_id = NEW.function_id
    FOR KEY SHARE;

    IF NOT FOUND
       OR current_return_type_kind IS DISTINCT FROM 'named'
          AND current_return_type_kind IS DISTINCT FROM 'reference' THEN
        RETURN NEW;
    END IF;

    IF current_return_type_kind = 'reference'
       AND current_return_target_type_id = decode('000000000000000000000000000000f3', 'hex') THEN
        RETURN NEW;
    END IF;

    PERFORM 1
    FROM _orna_kernel.catalogue_object_types AS target
    WHERE target.catalogue_revision_id = NEW.catalogue_revision_id
      AND target.type_id = current_return_target_type_id
    FOR KEY SHARE;

    IF NOT FOUND THEN
        RAISE EXCEPTION
            'catalogue function return target does not exist for catalogue revision'
            USING ERRCODE = 'foreign_key_violation';
    END IF;

    RETURN NEW;
END;
$return_target$;

REVOKE ALL ON FUNCTION _orna_kernel.validate_catalogue_function_return_target() FROM PUBLIC;

CREATE CONSTRAINT TRIGGER catalogue_functions_catalogue_revision_id_return_target_ty_fkey
    AFTER INSERT OR UPDATE OF return_type_kind, catalogue_revision_id, return_target_type_id
    ON _orna_kernel.catalogue_functions
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION _orna_kernel.validate_catalogue_function_return_target();


-- Replacing the two composite FKs removes their delete-side checks. Restore
-- that behavior for ordinary named/reference signatures while keeping the
-- sealed invocation carrier (which has no object row) outside the object table.
CREATE FUNCTION _orna_kernel.validate_catalogue_object_type_target_delete()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, _orna_kernel
AS $object_target_delete$
BEGIN
    -- A prior DELETE/UPDATE event is stale if the old parent key has been
    -- restored by the time this deferred trigger runs.
    PERFORM 1
    FROM _orna_kernel.catalogue_object_types AS current_target
    WHERE current_target.catalogue_revision_id = OLD.catalogue_revision_id
      AND current_target.type_id = OLD.type_id
    FOR KEY SHARE;

    IF FOUND THEN
        RETURN OLD;
    END IF;

    IF OLD.type_id = decode('000000000000000000000000000000f3', 'hex') THEN
        RETURN OLD;
    END IF;

    PERFORM 1
    FROM _orna_kernel.catalogue_function_parameters AS parameter
    WHERE parameter.catalogue_revision_id = OLD.catalogue_revision_id
      AND parameter.target_type_id = OLD.type_id
      AND parameter.type_kind IN ('named', 'reference')
    FOR KEY SHARE;

    IF FOUND THEN
        RAISE EXCEPTION
            'catalogue object type remains referenced by a function parameter'
            USING ERRCODE = 'foreign_key_violation';
    END IF;

    PERFORM 1
    FROM _orna_kernel.catalogue_functions AS function_row
    WHERE function_row.catalogue_revision_id = OLD.catalogue_revision_id
      AND function_row.return_target_type_id = OLD.type_id
      AND function_row.return_type_kind IN ('named', 'reference')
    FOR KEY SHARE;

    IF FOUND THEN
        RAISE EXCEPTION
            'catalogue object type remains referenced by a function return'
            USING ERRCODE = 'foreign_key_violation';
    END IF;

    RETURN OLD;
END;
$object_target_delete$;

REVOKE ALL ON FUNCTION _orna_kernel.validate_catalogue_object_type_target_delete() FROM PUBLIC;

CREATE CONSTRAINT TRIGGER catalogue_object_types_function_target_fkey
    AFTER DELETE OR UPDATE OF catalogue_revision_id, type_id
    ON _orna_kernel.catalogue_object_types
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION _orna_kernel.validate_catalogue_object_type_target_delete();
