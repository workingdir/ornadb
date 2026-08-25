-- Reject the all-zero PrincipalId sentinel at the durable security boundary.
-- Existing rows are checked while applying this migration, so tampered legacy
-- security state fails closed instead of being recovered as a real identity.
ALTER TABLE _orna_kernel.security_principals
    ADD CONSTRAINT security_principals_id_not_empty
    CHECK (id <> decode('00000000000000000000000000000000', 'hex'));
