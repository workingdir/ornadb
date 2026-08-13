CREATE TABLE _orna_kernel.security_local_peer_credentials (
    uid bigint PRIMARY KEY,
    principal_id bytea NOT NULL,
    CONSTRAINT security_local_peer_credentials_uid_range
        CHECK (uid BETWEEN 0 AND 4294967295),
    CONSTRAINT security_local_peer_credentials_principal_key
        UNIQUE (principal_id),
    CONSTRAINT security_local_peer_credentials_principal_fk
        FOREIGN KEY (principal_id)
        REFERENCES _orna_kernel.security_principals(id)
);

REVOKE ALL ON TABLE _orna_kernel.security_local_peer_credentials FROM PUBLIC;
