-- Preserve globally stable source-unit rows while recording every bundle that
-- retains each unit. Membership, rather than source_units.bundle_id, is the
-- authoritative immutable bundle composition.
CREATE TABLE _orna_kernel.source_bundle_units (
    bundle_id bytea NOT NULL REFERENCES _orna_kernel.source_bundles(id),
    source_unit_id bytea NOT NULL REFERENCES _orna_kernel.source_units(id),
    ordinal bigint NOT NULL CHECK (ordinal >= 0),
    PRIMARY KEY (bundle_id, source_unit_id),
    UNIQUE (bundle_id, ordinal)
);

-- The legacy source_units row points at its first owner. Carry that existing
-- composition into the authoritative membership relation before new writes use
-- it for shared units.
INSERT INTO _orna_kernel.source_bundle_units (bundle_id, source_unit_id, ordinal)
SELECT bundle_id, id, ordinal
FROM _orna_kernel.source_units
ORDER BY bundle_id, ordinal;

-- Do not silently bless databases that already lost a historical bundle. The
-- canonical empty bundle is the only valid non-member bundle.
CREATE TEMP TABLE _orna_migration_0043_membership_guard (
    valid boolean NOT NULL CHECK (valid)
) ON COMMIT DROP;

INSERT INTO _orna_migration_0043_membership_guard (valid)
SELECT false
FROM _orna_kernel.source_bundles AS bundle
WHERE bundle.content_hash <> decode(
    '965513f9c104e3c3fca13b46dcd382a64041a063d35ff0a316149bf5a4bfd641',
    'hex'
)
  AND NOT EXISTS (
      SELECT 1
      FROM _orna_kernel.source_bundle_units AS membership
      WHERE membership.bundle_id = bundle.id
  )
LIMIT 1;

REVOKE ALL ON TABLE _orna_kernel.source_bundle_units FROM PUBLIC;
