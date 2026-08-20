-- ADR 0078: reserve each resource request identity before target dispatch.
-- Rows are durable history, not a live-stream tombstone, so reuse is never allowed.
CREATE TABLE _orna_kernel.resource_request_history (
    request_id bytea PRIMARY KEY,
    reserved_at timestamp NOT NULL
        DEFAULT (transaction_timestamp() AT TIME ZONE 'UTC'),
    CONSTRAINT resource_request_history_request_id_length
        CHECK (octet_length(request_id) = 16)
);

REVOKE ALL ON TABLE _orna_kernel.resource_request_history FROM PUBLIC;
