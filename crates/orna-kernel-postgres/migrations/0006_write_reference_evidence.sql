-- Extend the persisted semantic-reference vocabulary for single-row writes.
-- Keep every existing target relation and compatibility clause unchanged.

ALTER TABLE _orna_kernel.definition_references
    DROP CONSTRAINT definition_references_reference_kind_check,
    DROP CONSTRAINT definition_references_reference_target_compatibility_check,
    ADD CONSTRAINT definition_references_reference_kind_check CHECK (reference_kind IN (
        'function_call',
        'named_type',
        'object_reference',
        'parameter_read',
        'query_object',
        'query_field',
        'expression',
        'write_object',
        'write_field'
    )),
    ADD CONSTRAINT definition_references_reference_target_compatibility_check CHECK (
        (reference_kind = 'function_call' AND target_kind = 'function')
        OR (
            reference_kind IN ('named_type', 'object_reference', 'query_object')
            AND target_kind = 'object_type'
        )
        OR (reference_kind = 'parameter_read' AND target_kind = 'parameter')
        OR (reference_kind = 'query_field' AND target_kind = 'field')
        OR (reference_kind = 'expression' AND target_kind = 'expression')
        OR (reference_kind = 'write_object' AND target_kind = 'object_type')
        OR (reference_kind = 'write_field' AND target_kind = 'field')
    );
