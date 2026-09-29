use orna_protocol_v1::{Envelope, Limits};

fn decode_hex(value: &str) -> Vec<u8> {
    assert_eq!(value.len() % 2, 0, "hex vector has odd length");
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let digit = |byte| match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                _ => panic!("invalid lowercase hexadecimal byte {byte:?}"),
            };
            digit(pair[0]) * 16 + digit(pair[1])
        })
        .collect()
}

#[test]
fn reference_protocol_vectors_decode_and_reencode_canonically() {
    let vectors: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../reference/Orna-1.0.0/tests/protocol-vectors.json"
    ))
    .expect("valid reference protocol vectors");
    let vectors = vectors.as_array().expect("protocol vectors are an array");
    assert!(
        !vectors.is_empty(),
        "reference protocol vectors are present"
    );

    for vector in vectors {
        let name = vector["name"].as_str().expect("vector name");
        let bytes = decode_hex(vector["hex"].as_str().expect("vector hex"));
        let envelope = Envelope::decode(&bytes, Limits::default())
            .unwrap_or_else(|error| panic!("{name}: decode failed: {error}"));
        let encoded = envelope
            .encode(Limits::default())
            .unwrap_or_else(|error| panic!("{name}: encode failed: {error}"));
        assert_eq!(encoded, bytes, "{name}: canonical round trip");
        println!("PASS protocol vector: {name}");
    }
}
