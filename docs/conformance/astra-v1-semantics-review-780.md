# Astra v1 semantics review (#780)

## Scope and evidence

This review compares the requested baseline `9cd3b76d` with the current review snapshot `8183e753` for local annotated lets, internal inference errors, nominal privacy, and nested protocol implementation conformance. Normative citations refer to the frozen Orna 1.0.0 source.

Baseline findings below are **static source traces**; tests were not run against the historical baseline. Current-state evidence is the actual semantic review integration suite and the focused generic-member probe. This is bounded semantic evidence, not full Orna 1.0.0 conformance evidence.

## Baseline findings and current disposition

| Area | Baseline evidence | Current evidence and disposition |
|---|---|---|
| Annotated local initializer | At `crates/orna-semantic-v1/src/lib.rs:3420-3423` in `9cd3b76d`, the initializer is inferred, then the annotation replaces its type before binding without a compatibility check. This admits `let x: Int = "wrong"`. | Current `infer_contextual` and `require_same` check the initializer before binding (`crates/orna-semantic-v1/src/lib.rs:3445-3452`). `local_annotated_initializer_mismatch_requires_type_diagnostic` passes. The gap is fixed. Norm: Algorithm INFER-1, steps 3-6 (`reference/Orna-1.0.0/source/05-types.md:456-464`). |
| Internal `Type::Error` escape | Unsupported products became `Type::Error` (`crates/orna-semantic-v1/src/lib.rs:1545-1558`); `types_match` treated either error sentinel as compatible (`crates/orna-semantic-v1/src/lib.rs:7620-7625`). Separately, field-only lambda inference inserted `Type::Error` for unknown field types (`crates/orna-semantic-v1/src/lib.rs:3717-3724`), allowing an underconstrained exported result. | Current annotation validation diagnoses unrepresentable products (`crates/orna-semantic-v1/src/lib.rs:5269-5286`), and the regression suite requires a targeted annotation for the underconstrained lambda fixture. `unsupported_product_annotation_requires_type_diagnostic` and `underconstrained_lambda_field_inference_requires_annotation` pass. The scoped gaps are fixed. Norms: ORNA-INFER-002 and ORNA-INFER-007 (`reference/Orna-1.0.0/source/05-types.md:313,323`). |
| Nominal identity and private representation | Baseline `infer_nominal` returned `Type::Record(expected.clone())` (`crates/orna-semantic-v1/src/lib.rs:4536-4589`), erasing nominal identity at factory boundaries and exposing a structural view to unrelated modules. | Current construction returns the nominal `constructor_type` (`crates/orna-semantic-v1/src/lib.rs:9253-9259`). `private_nominal_field_through_factory_requires_semantic_diagnostic` and nominal identity regressions pass. The scoped gap is fixed. Norms: ORNA-NOMINAL-002, -005, -006, and -007 (`reference/Orna-1.0.0/source/05-types.md:368,374,376,378`). |
| Nested implementation conformance | The baseline nominal-type pass checked overlap and selected special cases (`crates/orna-semantic-v1/src/lib.rs:2033-2062`) but did not compare required protocol members with implementation signatures. | Current local non-generic validation checks compatible signatures and missing members (`crates/orna-semantic-v1/src/lib.rs:3994-4021,4049-4077`). Tests for missing members and wrong parameter/return signatures pass. The non-generic local gap is fixed. Norm: ORNA-GENERIC-011 (`reference/Orna-1.0.0/source/06-expressions.md:181`). |

## Remaining ORNA-GENERIC-011 gap

The current validator explicitly omits generic protocol members: `crates/orna-semantic-v1/src/lib.rs:3770-3778,3951-3959,4049-4077`. The existing `mixed_generic_protocol_members_keep_their_checkable_surface` case (`crates/orna-semantic-v1/tests/v1_review_regressions.rs:1465-1482`) is accepted while omitting the required generic member:

```orna
pub protocol P {
    fn generic<T>(self, input: T): T;
    fn value(self): Int;
    static label: Str;
}
pub type Box {
    impl P {
        fn value(self): Int = 1;
        static label = "box";
    }
}
```

The probe passed, but that pass demonstrates the gap: the test expects acceptance despite the absent required member. ORNA-GENERIC-011 requires every required member to have exactly one compatible implementation and states no generic-member exception. The remaining issue is omission detection; this review makes no claim about generic signature substitution semantics.

## Captured current-state proof

Command: `cargo test --locked -p orna-semantic-v1 --test v1_review_regressions`

```text
running 79 tests
...
test result: ok. 79 passed; 0 failed; 0 ignored; 0 measured
```

Exit code: `0`.

Focused counterexample command: `cargo test --locked -p orna-semantic-v1 --test v1_review_regressions mixed_generic_protocol_members_keep_their_checkable_surface`

```text
running 1 test
test mixed_generic_protocol_members_keep_their_checkable_surface ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 78 filtered out
```

Exit code: `0`. The test assertion accepts the missing generic member, so this is not conformance evidence.

No semantic source or test files were changed. The review finding remains bounded to ORNA-GENERIC-011 required-member omission for generic members; full protocol and runtime conformance are not claimed.
