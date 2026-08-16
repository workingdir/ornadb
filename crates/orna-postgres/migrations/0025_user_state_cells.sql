-- ADR 0061 step 3: the durable per-principal USER state service stores one
-- typed cell per full logical key in _orna_kernel.user_state_cells. The key
-- is the complete logical tuple: changing any component creates a distinct
-- cell. The principal comes from the authenticated session, never a request.
--
-- An empty root_state_profile denotes the default state profile and an empty
-- function_instance_key denotes the default function instance; both are
-- accepted and stored as-is (no sentinel). The kernel model rejects NUL
-- bytes in these text components so they always round-trip through TEXT.
--
-- The typed value is stored as the canonical ORV5 encoding of the runtime
-- value (value_bytes) with the value TypeId. The revision is a strictly
-- positive monotonic counter owned by the kernel write path; the
-- expected-revision conflict protocol is a later step. The relation is
-- private to _orna_kernel with no public grants.

CREATE TABLE _orna_kernel.user_state_cells (
    principal_id bytea NOT NULL,
    root_function_id bytea NOT NULL,
    root_state_profile text NOT NULL,
    function_id bytea NOT NULL,
    function_instance_key text NOT NULL,
    state_slot_id bytea NOT NULL,
    value_bytes bytea NOT NULL,
    value_type_id bytea NOT NULL,
    revision bigint NOT NULL,
    updated_at timestamp with time zone NOT NULL
        DEFAULT transaction_timestamp(),
    CONSTRAINT user_state_cells_pkey
        PRIMARY KEY (
            principal_id,
            root_function_id,
            root_state_profile,
            function_id,
            function_instance_key,
            state_slot_id
        ),
    CONSTRAINT user_state_cells_identity_lengths CHECK (
        octet_length(principal_id) = 16
        AND octet_length(root_function_id) = 16
        AND octet_length(function_id) = 16
        AND octet_length(state_slot_id) = 16
        AND octet_length(value_type_id) = 16
    ),
    CONSTRAINT user_state_cells_revision_check CHECK (revision > 0)
);

-- The USER state load path returns every cell matching the authenticated
-- principal, the root function, and the state profile; the PK prefix already
-- orders by these columns, and this dedicated index keeps the scan stable.
CREATE INDEX user_state_cells_principal_root_state_profile_idx
    ON _orna_kernel.user_state_cells (principal_id, root_function_id, root_state_profile);

REVOKE ALL ON TABLE _orna_kernel.user_state_cells FROM PUBLIC;
