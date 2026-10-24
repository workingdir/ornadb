# Spec questions

## ornadb-5il1.420: is `sys.codec.unsupported_value_profile` ever wire-visible?

- Spec: `sys.codec.unsupported_value_profile` is defined at
  `reference/Orna-1.1.0/source/34-system-reference.md:3568` for the sys/value layers.
- Spec: `reference/Orna-1.1.0/source/30-protocol.md:119` closes the set of
  codes the protocol emits to `wire.*`.
- Ruling applied (Main): the sys code never reaches the wire. Protocol decode
  rejects OVB-2/ROV-3/SOV-3 profile tags (60110..=60113) with the existing
  `wire.invalid_message` outcome, before any effect.
- Spec-silent: whether the sys code should ever be wire-visible. Flagged for a
  spec decision; no protocol variant was added.
