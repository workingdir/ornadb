#[cfg(kani)]
mod proofs {
    use orna_value_v1::{Error, Value, base64_decode, base64_encode};

    /// INVARIANT: `kani_codec_invariants.rs:decode_total_on_bounded_bytes`:
    /// for every eight-byte input, OVB-1 decoding does not panic; an accepted
    /// value re-encodes to precisely the original bytes, and every rejection
    /// is a variant of the payload-free `Error` enum. This checks the strict
    /// canonical-decoder obligation of ORNA-FORMAT-001, including truncated,
    /// trailing, malformed and noncanonical inputs.
    /// ASSUMPTIONS: none.
    /// INPUT BOUNDS: exactly eight arbitrary bytes (`[u8; 8]`); each recursive
    /// `Reader::raw` invocation consumes at least one byte, so at most nine
    /// frames (depths 0..=8, with the last possibly failing on truncation).
    /// UNWIND: 24; `Reader::arg` reads at most four available argument bytes
    /// (an eight-byte argument cannot follow its one-byte head), and its
    /// array/map loops complete at most seven child items or attempt an eighth
    /// before failing. Their loop checks need at most nine unwinds; the nine
    /// `Reader::raw` frames, plus validation/re-encoding traversals, fit with
    /// slack under 24. Kani's default unwinding assertions remain enabled.
    #[kani::proof]
    #[kani::unwind(24)]
    fn decode_total_on_bounded_bytes() {
        let bytes: [u8; 8] = kani::any();
        match Value::decode(&bytes) {
            Ok(value) => assert_eq!(value.encode(), Ok(bytes.to_vec())),
            Err(error) => match error {
                Error::Truncated
                | Error::TrailingBytes
                | Error::Limit
                | Error::Unsupported
                | Error::NonCanonical
                | Error::InvalidUtf8
                | Error::InvalidTag
                | Error::InvalidValue
                | Error::DuplicateOrUnorderedMapKey
                | Error::ProtectedValue
                | Error::InvalidPath
                | Error::InvalidSchema
                | Error::DecimalLimit
                | Error::DivisionByZero
                | Error::NonFiniteDecimal => {}
            },
        }
    }

    fn assert_base64_roundtrip(bytes: &[u8]) {
        let encoded = base64_encode(bytes).expect("bounded Base64 encoding succeeds");
        assert_eq!(encoded.len() % 4, 0);
        assert_eq!(base64_decode(&encoded), Ok(bytes.to_vec()));
    }

    /// INVARIANT: `kani_codec_invariants.rs:base64_roundtrip_all_padding_classes`:
    /// every three-, four- and five-byte input encodes without panic or error
    /// to a multiple-of-four-length Base64 string and decodes byte-for-byte
    /// to the input. The lengths cover no padding, `==` and `=` respectively;
    /// this proves the padded standard-Base64 Blob rule in section 29's
    /// unnumbered canonical-text provision (adjacent to ORNA-FORMAT-004).
    /// ASSUMPTIONS: none.
    /// INPUT BOUNDS: three independent fixed arrays `[u8; 3]`, `[u8; 4]`
    /// and `[u8; 5]`, totaling twelve unconstrained symbolic bytes.
    /// UNWIND: 8; for each call the `base64_encode` chunks(3) loop and
    /// `base64_decode` chunks_exact(4) loop run at most ceil(5/3) = 2 times;
    /// their exit checks need 3 unwinds each. Eight allows slack for both
    /// padding branches while retaining default unwinding assertions.
    #[kani::proof]
    #[kani::unwind(8)]
    fn base64_roundtrip_all_padding_classes() {
        let p3: [u8; 3] = kani::any();
        let p4: [u8; 4] = kani::any();
        let p5: [u8; 5] = kani::any();
        assert_base64_roundtrip(&p3);
        assert_base64_roundtrip(&p4);
        assert_base64_roundtrip(&p5);
    }
}
