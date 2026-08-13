CREATE TABLE _orna_kernel.security_principals (
    id bytea PRIMARY KEY,
    kind text NOT NULL,
    status text NOT NULL,
    CONSTRAINT security_principals_id_length
        CHECK (octet_length(id) = 16),
    CONSTRAINT security_principals_kind_check
        CHECK (kind IN ('user', 'role', 'service')),
    CONSTRAINT security_principals_status_check
        CHECK (status IN ('active', 'disabled'))
);

CREATE TABLE _orna_kernel.security_role_memberships (
    role_id bytea NOT NULL,
    member_id bytea NOT NULL,
    PRIMARY KEY (role_id, member_id),
    CONSTRAINT security_role_memberships_not_self
        CHECK (role_id <> member_id),
    CONSTRAINT security_role_memberships_role_fk
        FOREIGN KEY (role_id)
        REFERENCES _orna_kernel.security_principals(id),
    CONSTRAINT security_role_memberships_member_fk
        FOREIGN KEY (member_id)
        REFERENCES _orna_kernel.security_principals(id)
);

CREATE INDEX security_role_memberships_member_index
    ON _orna_kernel.security_role_memberships(member_id, role_id);

CREATE TABLE _orna_kernel.security_execute_grants (
    grantee_id bytea NOT NULL,
    function_id bytea NOT NULL,
    PRIMARY KEY (grantee_id, function_id),
    CONSTRAINT security_execute_grants_function_id_length
        CHECK (octet_length(function_id) = 16),
    CONSTRAINT security_execute_grants_grantee_fk
        FOREIGN KEY (grantee_id)
        REFERENCES _orna_kernel.security_principals(id)
);

CREATE INDEX security_execute_grants_function_index
    ON _orna_kernel.security_execute_grants(function_id, grantee_id);

REVOKE ALL ON TABLE _orna_kernel.security_principals FROM PUBLIC;
REVOKE ALL ON TABLE _orna_kernel.security_role_memberships FROM PUBLIC;
REVOKE ALL ON TABLE _orna_kernel.security_execute_grants FROM PUBLIC;
