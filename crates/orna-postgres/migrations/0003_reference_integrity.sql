-- Close the persisted v1 semantic-reference vocabulary to definition kinds
-- with stable 16-byte identities and relations the core can validate.
-- Ordinal result columns remain catalogue subobjects, not reference targets.

ALTER TABLE _orna_kernel.definition_references
    DROP CONSTRAINT definition_references_target_kind_check,
    DROP CONSTRAINT definition_references_reference_kind_check,
    ADD CONSTRAINT definition_references_target_kind_check CHECK (target_kind IN (
        'object_type',
        'field',
        'function',
        'parameter',
        'expression'
    )),
    ADD CONSTRAINT definition_references_reference_kind_check CHECK (reference_kind IN (
        'function_call',
        'named_type',
        'object_reference',
        'parameter_read',
        'query_object',
        'query_field',
        'expression'
    ));
