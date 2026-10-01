#[cfg(kani)]
mod proofs {
    use std::cmp::Ordering;

    use orna_value_v1::{
        CANONICAL_NAN_BITS, Raw, Value, float_ordinary_eq, float_total_cmp,
    };

    /// INVARIANT: `kani_float_invariants.rs:float_bits_roundtrip_is_canonical`:
    /// for every binary64 bit pattern, OVB-1 encoding of `Value::float_bits(bits)`
    /// decodes to the identical Value; its raw payload is the exact input bits
    /// except that every NaN becomes `CANONICAL_NAN_BITS`. The bit-level NaN
    /// classification agrees with `f64::is_nan`. ORNA-FORMAT-001 (OVB-1) and
    /// the binary64/NaN canonical-encoding rule in 29-formats.md, §OVB-1.
    /// ASSUMPTIONS: none.
    /// INPUT BOUNDS: one arbitrary u64, all 2^64 binary64 bit patterns.
    /// UNWIND: 20; the Float branch has no collection loops, while its fixed
    /// eight-byte payload is copied/extended at most eight times per encode
    /// (including the decoder's canonical re-encode); 20 exceeds 8 + 1 and
    /// leaves slack for the fixed-size array and Vec iteration internals.
    #[kani::proof]
    #[kani::unwind(20)]
    fn float_bits_roundtrip_is_canonical() {
        let bits = kani::any::<u64>();
        let is_nan = bits & 0x7ff0_0000_0000_0000 == 0x7ff0_0000_0000_0000
            && bits & 0x000f_ffff_ffff_ffff != 0;
        assert_eq!(is_nan, f64::from_bits(bits).is_nan());

        let value = Value::float_bits(bits);
        let encoded = value.encode().expect("every canonical Float encodes");
        let decoded = Value::decode(&encoded).expect("encoded Float decodes");
        assert_eq!(decoded, value);

        if is_nan {
            assert_eq!(value, Value::float_bits(CANONICAL_NAN_BITS));
            assert_eq!(value.raw(), &Raw::Float(CANONICAL_NAN_BITS));
        } else {
            assert_eq!(value.raw(), &Raw::Float(bits));
        }
    }

    /// INVARIANT: `kani_float_invariants.rs:float_total_order_matches_std`:
    /// for every pair of bit patterns, FLOAT-TOTAL-1/ORNA-FLOAT-002 total
    /// ordering equals Rust binary64 `total_cmp`; ORNA-FLOAT-001 and
    /// ORNA-FLOAT-006 ordinary equality equals IEEE numeric equality excluding
    /// NaNs. Total comparison is reflexive and antisymmetric; negative zero
    /// precedes positive zero in total order, but they are ordinarily equal.
    /// ASSUMPTIONS: none.
    /// INPUT BOUNDS: two arbitrary u64 values, all 2^128 ordered pairs.
    /// UNWIND: 4; the comparisons contain no input-dependent loops, so one
    /// iteration plus slack covers their scalar branches.
    #[kani::proof]
    #[kani::unwind(4)]
    fn float_total_order_matches_std() {
        let a = kani::any::<u64>();
        let b = kani::any::<u64>();
        let left = f64::from_bits(a);
        let right = f64::from_bits(b);

        assert_eq!(float_total_cmp(a, b), left.total_cmp(&right));
        assert_eq!(
            float_ordinary_eq(a, b),
            !left.is_nan() && !right.is_nan() && left == right
        );
        assert_eq!(float_total_cmp(a, a), Ordering::Equal);
        assert_eq!(float_total_cmp(a, b), float_total_cmp(b, a).reverse());
        assert_eq!(
            float_total_cmp((-0.0f64).to_bits(), 0.0f64.to_bits()),
            Ordering::Less
        );
        assert_eq!(
            float_total_cmp(0.0f64.to_bits(), (-0.0f64).to_bits()),
            Ordering::Greater
        );
        assert!(float_ordinary_eq(0.0f64.to_bits(), (-0.0f64).to_bits()));
    }
}
