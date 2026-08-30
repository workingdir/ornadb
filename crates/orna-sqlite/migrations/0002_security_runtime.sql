CREATE TABLE IF NOT EXISTS orna_security_principals (
    principal_id BLOB NOT NULL PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind IN ('user', 'role', 'service')),
    status TEXT NOT NULL CHECK (status IN ('active', 'disabled')),
    CHECK (length(principal_id) = 16)
);

CREATE TABLE IF NOT EXISTS orna_security_role_memberships (
    role_id BLOB NOT NULL,
    member_id BLOB NOT NULL,
    PRIMARY KEY (role_id, member_id),
    CHECK (length(role_id) = 16),
    CHECK (length(member_id) = 16),
    CHECK (role_id <> member_id),
    FOREIGN KEY (role_id) REFERENCES orna_security_principals(principal_id),
    FOREIGN KEY (member_id) REFERENCES orna_security_principals(principal_id)
);

CREATE TABLE IF NOT EXISTS orna_security_execute_grants (
    grantee_id BLOB NOT NULL,
    function_id BLOB NOT NULL,
    PRIMARY KEY (grantee_id, function_id),
    CHECK (length(grantee_id) = 16),
    CHECK (length(function_id) = 16),
    FOREIGN KEY (grantee_id) REFERENCES orna_security_principals(principal_id)
);

CREATE TABLE IF NOT EXISTS orna_security_local_peer_credentials (
    uid INTEGER NOT NULL PRIMARY KEY,
    principal_id BLOB NOT NULL UNIQUE,
    CHECK (uid >= 0),
    CHECK (length(principal_id) = 16),
    FOREIGN KEY (principal_id) REFERENCES orna_security_principals(principal_id)
);

CREATE TABLE IF NOT EXISTS orna_security_privilege_grants (
    grantee_id BLOB NOT NULL,
    privilege TEXT NOT NULL,
    object_id BLOB,
    PRIMARY KEY (grantee_id, privilege, object_id),
    CHECK (length(grantee_id) = 16),
    CHECK (object_id IS NULL OR length(object_id) = 16),
    CHECK (
        (privilege = 'security_admin' AND object_id IS NULL)
        OR privilege = 'execute'
        OR privilege IN (
            'inspect:own-invocation',
            'inspect:session-invocations',
            'inspect:any-invocation',
            'inspect:values',
            'inspect:source',
            'inspect:security-details',
            'inspect:runtime-internals'
        )
    ),
    FOREIGN KEY (grantee_id) REFERENCES orna_security_principals(principal_id)
);

CREATE UNIQUE INDEX IF NOT EXISTS orna_security_privilege_grants_key
    ON orna_security_privilege_grants (
        grantee_id,
        privilege,
        COALESCE(object_id, X'00000000000000000000000000000000')
    );


CREATE TABLE IF NOT EXISTS orna_invocation_audit_events (
    invocation_id BLOB NOT NULL PRIMARY KEY,
    recorded_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    outcome TEXT NOT NULL CHECK (outcome IN ('allowed', 'denied', 'completed', 'failed')),
    session_principal_id BLOB NOT NULL,
    effective_principal_id BLOB,
    authorising_principal_id BLOB,
    function_id BLOB,
    source_revision_id BLOB,
    catalogue_revision_id BLOB,
    error_code TEXT,
    CHECK (length(invocation_id) = 16),
    CHECK (length(session_principal_id) = 16),
    CHECK (effective_principal_id IS NULL OR length(effective_principal_id) = 16),
    CHECK (authorising_principal_id IS NULL OR length(authorising_principal_id) = 16),
    CHECK (function_id IS NULL OR length(function_id) = 16),
    CHECK (source_revision_id IS NULL OR length(source_revision_id) = 16),
    CHECK (catalogue_revision_id IS NULL OR length(catalogue_revision_id) = 16)
);

CREATE TABLE IF NOT EXISTS orna_user_state_cells (
    principal_id BLOB NOT NULL,
    root_function_id BLOB NOT NULL,
    root_state_profile TEXT NOT NULL,
    function_id BLOB NOT NULL,
    function_instance_key TEXT NOT NULL,
    state_slot_id BLOB NOT NULL,
    value_bytes BLOB NOT NULL,
    value_type_id BLOB NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (
        principal_id,
        root_function_id,
        root_state_profile,
        function_id,
        function_instance_key,
        state_slot_id
    ),
    CHECK (length(principal_id) = 16),
    CHECK (length(root_function_id) = 16),
    CHECK (length(function_id) = 16),
    CHECK (length(state_slot_id) = 16),
    CHECK (length(value_type_id) = 16),
    CHECK (length(root_state_profile) <= 1024),
    CHECK (length(function_instance_key) <= 1024)
);

CREATE TABLE IF NOT EXISTS orna_inspect_snapshots (
    epoch_id BLOB NOT NULL PRIMARY KEY,
    invocation_id BLOB NOT NULL UNIQUE,
    recorded_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    owner_principal_id BLOB NOT NULL,
    source_revision_id BLOB NOT NULL,
    catalogue_revision_id BLOB NOT NULL,
    summary_bytes BLOB NOT NULL,
    CHECK (length(epoch_id) = 16),
    CHECK (length(invocation_id) = 16),
    CHECK (length(owner_principal_id) = 16),
    CHECK (length(source_revision_id) = 16),
    CHECK (length(catalogue_revision_id) = 16),
    CHECK (length(summary_bytes) <= 16777216)
);

CREATE TABLE IF NOT EXISTS orna_inspect_trace_events (
    invocation_id BLOB NOT NULL,
    sequence INTEGER NOT NULL,
    kind TEXT NOT NULL,
    payload_bytes BLOB NOT NULL,
    observer_invocation_id BLOB,
    recorded_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (invocation_id, sequence),
    CHECK (sequence >= 0),
    CHECK (length(invocation_id) = 16),
    CHECK (observer_invocation_id IS NULL OR length(observer_invocation_id) = 16),
    CHECK (length(payload_bytes) <= 16777216)
);

CREATE TABLE IF NOT EXISTS orna_user_state_audit_events (
    audit_id BLOB NOT NULL PRIMARY KEY,
    recorded_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    operation TEXT NOT NULL CHECK (operation IN ('load', 'write')),
    outcome TEXT NOT NULL CHECK (outcome IN ('completed', 'conflict')),
    session_principal_id BLOB NOT NULL,
    root_function_id BLOB NOT NULL,
    root_state_profile TEXT NOT NULL,
    cell_count INTEGER NOT NULL CHECK (cell_count >= 0),
    CHECK (length(audit_id) = 16),
    CHECK (length(session_principal_id) = 16),
    CHECK (length(root_function_id) = 16),
    CHECK (length(root_state_profile) <= 1024)
);
