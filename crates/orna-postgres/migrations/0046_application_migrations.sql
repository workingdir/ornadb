CREATE SCHEMA IF NOT EXISTS _orna_kernel;

CREATE TABLE _orna_kernel.application_migrations (
    ordinal bigint NOT NULL,
    format text NOT NULL,
    version bigint NOT NULL,
    expected_source_revision_id bytea NOT NULL,
    expected_catalogue_revision_id bytea NOT NULL,
    candidate_source_revision_id bytea NOT NULL,
    candidate_catalogue_revision_id bytea NOT NULL,
    canonical_bytes bytea NOT NULL,
    digest bytea NOT NULL,
    applied_at timestamp with time zone NOT NULL DEFAULT transaction_timestamp(),
    PRIMARY KEY (ordinal),
    UNIQUE (candidate_source_revision_id, candidate_catalogue_revision_id),
    CHECK (ordinal >= 0),
    CHECK (length(format) > 0),
    CHECK (version > 0),
    CHECK (octet_length(expected_source_revision_id) = 16),
    CHECK (octet_length(expected_catalogue_revision_id) = 16),
    CHECK (octet_length(candidate_source_revision_id) = 16),
    CHECK (octet_length(candidate_catalogue_revision_id) = 16),
    CHECK (octet_length(canonical_bytes) > 0),
    CHECK (octet_length(digest) = 32)
);

REVOKE ALL ON SCHEMA _orna_kernel FROM PUBLIC;

REVOKE ALL ON TABLE _orna_kernel.application_migrations FROM PUBLIC;
