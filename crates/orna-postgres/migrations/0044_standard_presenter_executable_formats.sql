-- ADR: retain the V8 standard presenter executables in the durable artifact
-- relation. V8 adds terminal-table and CSV presenters alongside the formats
-- already retained by earlier standard-library revisions.
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
                    'orna.server-json-encode',
                    'orna.server-terminal-table',
                    'orna.server-csv-encode'
                ))
            OR (artifact_kind = 'client_bytecode' AND format = 'orna.client-plan')
        );
