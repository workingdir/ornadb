use orna_value_v1::{decode_typed, encode_typed, Error};

fn hex_bytes(input: &str) -> Vec<u8> {
    hex::decode(input).expect("valid test vector hex")
}

#[test]
fn uuid_codec_emits_tag_37_with_network_order_bytes() {
    let value = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
        0xee, 0xff,
    ];
    let expected = hex_bytes("d8255000112233445566778899aabbccddeeff");

    assert_eq!(encode_typed(&value).expect("typed UUID encoding succeeds"), expected);
    assert_eq!(decode_typed::<[u8; 16]>(&expected), Ok(value));
}

#[test]
fn uuid_codec_rejects_other_ovb_values_at_the_uuid_path() {
    let error = decode_typed::<[u8; 16]>(&hex_bytes("63616263"))
        .expect_err("text is not a UUID value");

    assert_eq!(error.path(), &["Uuid".to_owned()]);
    assert_eq!(error.type_path(), "Uuid");
    assert_eq!(error.error(), &Error::InvalidValue);
}

#[test]
fn uuid_codec_rejects_noncanonical_length_before_typed_decoding() {
    let error = decode_typed::<[u8; 16]>(&hex_bytes("d8254f000102030405060708090a0b0c0d0e"))
        .expect_err("a 15-byte UUID payload is malformed");

    assert_eq!(error.path(), &["Uuid".to_owned()]);
    assert_eq!(error.error(), &Error::InvalidTag);
}
