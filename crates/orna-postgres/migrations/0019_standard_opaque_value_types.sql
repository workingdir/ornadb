ALTER TABLE _orna_kernel.standard_catalogue_value_types
    DROP CONSTRAINT std_cat_value_types_value_kind_check,
    ADD CONSTRAINT std_cat_value_types_value_kind_check
        CHECK (value_kind IN ('primitive', 'opaque')),
    ADD CONSTRAINT std_cat_value_types_opaque_contract_check CHECK (
        value_kind <> 'opaque'
        OR (
            persistence = 'transient'
            AND octet_length(representation_contract) <= 128
            AND representation_contract !~ '[^ -~]'
        )
    );
