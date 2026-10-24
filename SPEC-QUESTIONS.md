# Spec questions

## ornadb-5il1.420: is `sys.codec.unsupported_value_profile` ever wire-visible?

- Spec wording, `reference/Orna-1.1.0/source/34-system-reference.md:3568`:
  `sys.codec.unsupported_value_profile` = "value profile not accepted by
  selected encoding or wire context".
- Spec, `reference/Orna-1.1.0/source/30-protocol.md:119`: the closed set of
  codes the protocol emits is `wire.*` only.
- Choice (deviation from the 34-system-reference.md:3568 wording, which names
  the "wire context"): the protocol keeps `wire.invalid_message` for OVB-2/ROV-3/SOV-3
  profile tags 60110..=60113, per the 30-protocol.md:119 closed set. No protocol
  variant carries the sys code.
- Applied (Main ruling, W143/W153): profile tags are rejected at decode, before any
  effect, at every value position: event value, patch value, relation/key path,
  extension value, present property.
- Spec-silent: whether the sys code should ever be wire-visible. Pending
  spec-owner confirmation.
