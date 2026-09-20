use orna_value_v1::{base64_decode, base64_encode, Error};

#[test]
fn encodes_and_decodes_rfc4648_boundary_vectors() {
    let vectors: &[(&[u8], &str)] = &[
        (b"", ""),
        (b"f", "Zg=="),
        (b"fo", "Zm8="),
        (b"foo", "Zm9v"),
        (&[0x00], "AA=="),
        (&[0xff, 0x10], "/xA="),
        (&[0x00, 0xff, 0x10], "AP8Q"),
    ];

    for &(bytes, encoded) in vectors {
        assert_eq!(base64_encode(bytes).unwrap(), encoded);
        assert_eq!(base64_decode(encoded), Ok(bytes.to_vec()));
        assert_eq!(base64_encode(&base64_decode(encoded).unwrap()).unwrap(), encoded);
    }
}

#[test]
fn rejects_malformed_lengths_and_padding() {
    for input in [
        "A",
        "AAA",
        "AAAAA",
        "=",
        "===",
        "====",
        "A===",
        "Z===",
        "Zg=",
        "Zg===",
        "Zg====",
        "=Zg=",
        "Z=g=",
        "Zm=8",
        "AAAA====",
    ] {
        assert!(matches!(base64_decode(input), Err(Error::InvalidValue)));
    }
}

#[test]
fn rejects_invalid_characters_whitespace_and_url_safe_alphabet() {
    for input in [
        "Zg$=",
        "Zg.=",
        "Zg\0=",
        "éA==",
        " Zg==",
        "Zg== ",
        "Zg\n==",
        "Zg\t==",
        "_w==",
        "-w==",
    ] {
        assert!(matches!(base64_decode(input), Err(Error::InvalidValue)));
    }
}

#[test]
fn rejects_nonzero_unused_trailing_bits() {
    assert!(matches!(base64_decode("Zh=="), Err(Error::InvalidValue)));
    assert!(matches!(base64_decode("Zm9="), Err(Error::InvalidValue)));
}
