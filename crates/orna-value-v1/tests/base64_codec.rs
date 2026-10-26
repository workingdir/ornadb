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
        assert_eq!(base64_encode(bytes), encoded);
        assert_eq!(base64_decode(encoded), Ok(bytes.to_vec()));
        assert_eq!(base64_encode(&base64_decode(encoded).unwrap()), encoded);
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

#[test]
fn strict_accept_table_is_the_workspace_reference() {
    // Same inputs as the live, evaluator and compact decoder tables.
    let cases: &[(&str, bool)] = &[
        ("", true),
        ("Zm8=", true),
        ("Zm9v", true),
        ("+/8=", true),
        ("-_8=", false),
        ("+_8=", false),
        ("-/8=", false),
        ("Zm8", false),
        ("Zm9v=", false),
        ("Zm9v==", false),
        ("Zm8= ", false),
        (" Zm8=", false),
        ("Zh==", false),
        ("Zm9=", false),
        ("====", false),
        ("Zg==Zm9v", false),
    ];
    for &(input, accepted) in cases {
        assert_eq!(base64_decode(input).is_ok(), accepted, "input {input:?}");
    }
}

#[test]
fn long_inputs_follow_the_same_rules() {
    let valid = "Zm9v".repeat(65_536);
    assert_eq!(base64_decode(&valid).map(|bytes| bytes.len()), Ok(196_608));
    assert_eq!(base64_encode(&vec![0xfb; 196_608]).len(), 262_144);
    let invalid = format!("{}-_8=", "Zm9v".repeat(65_536));
    assert!(matches!(base64_decode(&invalid), Err(Error::InvalidValue)));
}
