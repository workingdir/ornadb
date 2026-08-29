CREATE TABLE IF NOT EXISTS orna_active_revision (
    singleton INTEGER NOT NULL,
    source_revision_id BLOB NOT NULL,
    source_parent_revision_id BLOB,
    catalogue_revision_id BLOB NOT NULL,
    source_bundle_id BLOB NOT NULL,
    source_bundle_hash BLOB NOT NULL,
    source_revision_hash BLOB NOT NULL,
    catalogue_hash BLOB NOT NULL,
    PRIMARY KEY (singleton),
    UNIQUE (source_revision_id),
    UNIQUE (catalogue_revision_id),
    CHECK (singleton = 1),
    CHECK (length(source_revision_id) = 16),
    CHECK (source_parent_revision_id IS NULL OR length(source_parent_revision_id) = 16),
    CHECK (length(catalogue_revision_id) = 16),
    CHECK (length(source_bundle_id) = 16),
    CHECK (length(source_bundle_hash) = 32),
    CHECK (length(source_revision_hash) = 32),
    CHECK (length(catalogue_hash) = 32)
);

CREATE TABLE IF NOT EXISTS orna_source_revisions (
    source_revision_id BLOB NOT NULL,
    source_parent_revision_id BLOB,
    source_bundle_id BLOB NOT NULL,
    source_bundle_hash BLOB NOT NULL,
    source_revision_hash BLOB NOT NULL,
    PRIMARY KEY (source_revision_id),
    UNIQUE (source_bundle_id),
    CHECK (length(source_revision_id) = 16),
    CHECK (source_parent_revision_id IS NULL OR length(source_parent_revision_id) = 16),
    CHECK (length(source_bundle_id) = 16),
    CHECK (length(source_bundle_hash) = 32),
    CHECK (length(source_revision_hash) = 32)
);

CREATE TABLE IF NOT EXISTS orna_catalogue_revisions (
    catalogue_revision_id BLOB NOT NULL,
    source_revision_id BLOB NOT NULL,
    parent_catalogue_revision_id BLOB,
    catalogue_hash BLOB NOT NULL,
    PRIMARY KEY (catalogue_revision_id),
    CHECK (length(catalogue_revision_id) = 16),
    CHECK (length(source_revision_id) = 16),
    CHECK (
        parent_catalogue_revision_id IS NULL
        OR length(parent_catalogue_revision_id) = 16
    ),
    CHECK (length(catalogue_hash) = 32)
);

CREATE TABLE IF NOT EXISTS orna_source_units (
    source_revision_id BLOB NOT NULL,
    source_unit_id BLOB NOT NULL,
    ordinal INTEGER NOT NULL,
    logical_path TEXT NOT NULL,
    content TEXT NOT NULL,
    content_hash BLOB NOT NULL,
    PRIMARY KEY (source_revision_id, source_unit_id),
    UNIQUE (source_unit_id),
    UNIQUE (source_revision_id, ordinal),
    UNIQUE (source_revision_id, logical_path),
    CHECK (length(source_unit_id) = 16),
    CHECK (ordinal >= 0),
    CHECK (length(content_hash) = 32)
);

CREATE TABLE IF NOT EXISTS orna_catalogue_schemas (
    catalogue_revision_id BLOB NOT NULL,
    schema_id BLOB NOT NULL,
    name_parts TEXT NOT NULL,
    source_unit_id BLOB NOT NULL,
    source_start INTEGER NOT NULL,
    source_end INTEGER NOT NULL,
    PRIMARY KEY (catalogue_revision_id, schema_id),
    CHECK (length(schema_id) = 16),
    CHECK (length(source_unit_id) = 16),
    CHECK (source_start >= 0),
    CHECK (source_end >= source_start)
);

CREATE TABLE IF NOT EXISTS orna_application_migrations (
    ordinal INTEGER NOT NULL,
    format TEXT NOT NULL,
    version INTEGER NOT NULL,
    expected_source_revision_id BLOB NOT NULL,
    expected_catalogue_revision_id BLOB NOT NULL,
    candidate_source_revision_id BLOB NOT NULL,
    candidate_catalogue_revision_id BLOB NOT NULL,
    canonical_bytes BLOB NOT NULL,
    digest BLOB NOT NULL,
    applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (ordinal),
    UNIQUE (candidate_source_revision_id, candidate_catalogue_revision_id),
    CHECK (ordinal >= 0),
    CHECK (length(format) > 0),
    CHECK (version > 0),
    CHECK (length(expected_source_revision_id) = 16),
    CHECK (length(expected_catalogue_revision_id) = 16),
    CHECK (length(candidate_source_revision_id) = 16),
    CHECK (length(candidate_catalogue_revision_id) = 16),
    CHECK (length(canonical_bytes) > 0),
    CHECK (length(digest) = 32)
);
