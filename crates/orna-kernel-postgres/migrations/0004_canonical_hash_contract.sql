-- Bind every persisted content hash to the shared, domain-separated Orna
-- canonical hash contract. The bootstrap migration data step rewrites the
-- only supported legacy state before this migration is recorded.

ALTER TABLE _orna_kernel.source_units
    ADD COLUMN hash_contract_version smallint NOT NULL DEFAULT 1,
    ADD CONSTRAINT source_units_hash_contract_version_check
        CHECK (hash_contract_version = 1);

ALTER TABLE _orna_kernel.source_bundles
    ADD COLUMN hash_contract_version smallint NOT NULL DEFAULT 1,
    ADD CONSTRAINT source_bundles_hash_contract_version_check
        CHECK (hash_contract_version = 1);

ALTER TABLE _orna_kernel.source_revisions
    ADD COLUMN hash_contract_version smallint NOT NULL DEFAULT 1,
    ADD CONSTRAINT source_revisions_hash_contract_version_check
        CHECK (hash_contract_version = 1);

ALTER TABLE _orna_kernel.catalogue_revisions
    ADD COLUMN hash_contract_version smallint NOT NULL DEFAULT 1,
    ADD CONSTRAINT catalogue_revisions_hash_contract_version_check
        CHECK (hash_contract_version = 1);

ALTER TABLE _orna_kernel.catalogue_expressions
    ADD COLUMN hash_contract_version smallint NOT NULL DEFAULT 1,
    ADD CONSTRAINT catalogue_expressions_hash_contract_version_check
        CHECK (hash_contract_version = 1);

ALTER TABLE _orna_kernel.function_revisions
    ADD COLUMN hash_contract_version smallint NOT NULL DEFAULT 1,
    ADD CONSTRAINT function_revisions_hash_contract_version_check
        CHECK (hash_contract_version = 1);

ALTER TABLE _orna_kernel.function_artifacts
    ADD COLUMN hash_contract_version smallint NOT NULL DEFAULT 1,
    ADD CONSTRAINT function_artifacts_hash_contract_version_check
        CHECK (hash_contract_version = 1);

-- A revision's immutable declaration origin remains normalized through its
-- introduced_catalogue_revision_id/function_id foreign key to the immutable
-- introduction catalogue_functions row. A later catalogue may reuse the
-- revision while recording a different current definition origin.
-- The same declaration bytes can resolve to different executable semantics
-- after a dependency changes. Both hashes therefore identify an immutable
-- function revision.
ALTER TABLE _orna_kernel.function_revisions
    DROP CONSTRAINT function_revisions_function_id_content_hash_key,
    ADD CONSTRAINT function_revisions_function_content_semantic_key
        UNIQUE (function_id, content_hash, semantic_ir_hash);
