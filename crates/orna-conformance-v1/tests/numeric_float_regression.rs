use orna_value_v1::{CANONICAL_NAN_BITS, float_max, float_min};

#[test]
fn float_extrema_canonicalize_any_nan_payload() {
    let negative_signaling_nan = 0xfff0_0000_0000_0001;
    let positive_quiet_nan = 0x7ff8_0000_0000_0042;
    let one = 1.0f64.to_bits();

    // ORNA-FLOAT-007 requires canonical NaN propagation regardless of the
    // input NaN sign, signaling bit, or payload.
    assert_eq!(
        float_min(&[one, negative_signaling_nan]),
        Some(CANONICAL_NAN_BITS)
    );
    assert_eq!(
        float_max(&[positive_quiet_nan, one]),
        Some(CANONICAL_NAN_BITS)
    );
}
