-- Retain the exact runtime writer lease observed by executable composition at
-- sealed admission. Existing rows remain readable as legacy provenance, but
-- every newly resolved row must carry both fields. These fields are evidence
-- for a future cross-store compare-and-set recovery operation; they are not a
-- claim that this PostgreSQL transaction can itself fence the runtime lease.
ALTER TABLE _orna_kernel.sealed_invocation_lifecycle
    ADD COLUMN admission_writer_lease_owner bytea,
    ADD COLUMN admission_writer_lease_epoch bigint,
    ADD CONSTRAINT sealed_invocation_lifecycle_writer_lease_shape_check CHECK (
        (
            admission_writer_lease_owner IS NULL
            AND admission_writer_lease_epoch IS NULL
        )
        OR (
            admission_writer_lease_owner IS NOT NULL
            AND admission_writer_lease_epoch IS NOT NULL
            AND octet_length(admission_writer_lease_owner) = 16
            AND admission_writer_lease_owner <> decode(
                '00000000000000000000000000000000',
                'hex'
            )
            AND admission_writer_lease_epoch > 0
        )
    ) NOT VALID;

CREATE INDEX sealed_invocation_lifecycle_writer_lease_index
    ON _orna_kernel.sealed_invocation_lifecycle(
        admission_writer_lease_owner,
        admission_writer_lease_epoch
    )
    WHERE admission_writer_lease_owner IS NOT NULL;
