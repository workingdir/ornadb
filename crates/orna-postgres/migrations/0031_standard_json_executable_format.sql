-- ADR 0075: allow the retained V5 JSON presenter executable in the
-- standard executable relation.
ALTER TABLE _orna_kernel.standard_function_artifacts
    DROP CONSTRAINT std_fn_artifacts_format_check;

ALTER TABLE _orna_kernel.standard_function_artifacts
    ADD CONSTRAINT std_fn_artifacts_format_check
        CHECK (
            (artifact_kind = 'server_plan'
                AND format IN (
                    'orna.server-plan',
                    'orna.server-mutation-plan',
                    'orna.server-parameter-echo',
                    'orna.server-json-encode'
                ))
            OR (artifact_kind = 'client_bytecode' AND format = 'orna.client-plan')
        );
