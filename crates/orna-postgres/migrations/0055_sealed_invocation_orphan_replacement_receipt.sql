-- A terminal orphan transition retains the exact replacement lease presented
-- with its PostgreSQL compare-and-set. This is a PostgreSQL receipt, not an
-- assertion that PostgreSQL atomically fenced an external runtime lease.
-- Existing orphan rows predate this receipt and remain readable, but cannot be
-- acknowledged as an idempotent owner-replacement recovery.
ALTER TABLE _orna_kernel.sealed_invocation_lifecycle
    ADD COLUMN orphaned_replacement_writer_lease_owner bytea,
    ADD COLUMN orphaned_replacement_writer_lease_epoch bigint,
    ADD CONSTRAINT sealed_invocation_lifecycle_orphan_replacement_receipt_check CHECK (
        (
            orphaned_replacement_writer_lease_owner IS NULL
            AND orphaned_replacement_writer_lease_epoch IS NULL
        )
        OR (
            status = 'orphaned'
            AND admission_writer_lease_owner IS NOT NULL
            AND admission_writer_lease_epoch IS NOT NULL
            AND octet_length(orphaned_replacement_writer_lease_owner) = 16
            AND orphaned_replacement_writer_lease_owner <> decode(
                '00000000000000000000000000000000',
                'hex'
            )
            AND orphaned_replacement_writer_lease_epoch > admission_writer_lease_epoch
        )
    ) NOT VALID;
