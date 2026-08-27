-- ADR 0080: bind request-created inspection epochs to their trusted observer
-- execution context. Existing auto-captured snapshots remain legacy rows with
-- all three columns NULL.
ALTER TABLE _orna_kernel.inspect_snapshots
    ADD COLUMN observer_root_invocation_id bytea,
    ADD COLUMN observer_parent_invocation_id bytea,
    ADD COLUMN observer_purpose text,
    ADD CONSTRAINT inspect_snapshots_observer_context_check CHECK (
        (
            observer_root_invocation_id IS NULL
            AND observer_parent_invocation_id IS NULL
            AND observer_purpose IS NULL
        ) OR (
            octet_length(observer_root_invocation_id) = 16
            AND octet_length(observer_parent_invocation_id) = 16
            AND observer_root_invocation_id <> decode('00000000000000000000000000000000', 'hex')
            AND observer_parent_invocation_id <> decode('00000000000000000000000000000000', 'hex')
            AND observer_purpose = 'inspect'
        )
    );
