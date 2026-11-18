//! The typed `sys.GitOid` of `orna-value`: a Git object id and its tag-60023 value
//! `["sys.GitOid", ["sha1", bytes20]]` or `["sys.GitOid", ["sha256", bytes32]]`
//! (`source/29-formats.md`, "Portable opaque and contextual values").

use std::collections::HashSet;

use orna_value::{
    CanonicalValue, GIT_OID_TYPE_NAME, GitHash, GitObjectId, OPAQUE_SYSTEM_ID_TAG, OvbRaw,
    SystemReferenceError,
};

const SHA1_HEX: &str = "3f786850e387550fdab836ed7e6dc881de23001b";

fn sha256_hex() -> String {
    "c0".repeat(32)
}

fn opaque(name: &str, representation: OvbRaw) -> OvbRaw {
    OvbRaw::Tag(
        OPAQUE_SYSTEM_ID_TAG,
        Box::new(OvbRaw::Array(vec![
            OvbRaw::Text(name.to_owned()),
            representation,
        ])),
    )
}

fn git(algorithm: &str, bytes: Vec<u8>) -> OvbRaw {
    OvbRaw::Array(vec![
        OvbRaw::Text(algorithm.to_owned()),
        OvbRaw::Bytes(bytes),
    ])
}

fn bytes_of(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("hexadecimal"))
        .collect()
}

// Specified: ORNA-SYS-016; Orna-1.1.0 source/29-formats.md "Portable opaque and contextual values":
// `sys.GitOid` uses `["sha1", bytes20]` or `["sha256", bytes32]` in tag 60023.
#[test]
fn a_git_oid_is_the_tag_60023_value_of_its_algorithm_and_bytes() {
    let sha1 = GitObjectId::from_hex(SHA1_HEX).expect("a SHA-1 id");
    assert_eq!(sha1.algorithm(), GitHash::Sha1);
    assert_eq!(
        sha1.to_opaque_raw(),
        opaque(GIT_OID_TYPE_NAME, git("sha1", bytes_of(SHA1_HEX)))
    );
    let sha256 = GitObjectId::from_hex(&sha256_hex()).expect("a SHA-256 id");
    assert_eq!(sha256.algorithm(), GitHash::Sha256);
    assert_eq!(
        sha256.to_opaque_raw(),
        opaque(GIT_OID_TYPE_NAME, git("sha256", bytes_of(&sha256_hex())))
    );
    assert_eq!(GIT_OID_TYPE_NAME, "sys.GitOid");
}

// Specified: ORNA-SYS-016, ORNA-FORMAT-001; the encoding is deterministic: tag 60023 is
// `d9 ea 77` and its payload is the two-item array of the type name and the representation.
#[test]
fn a_git_oid_has_one_canonical_encoding_that_decodes_back_to_it() {
    let sha1 = GitObjectId::new(GitHash::Sha1, vec![0; 20]).expect("a SHA-1 id");
    let mut golden = bytes_of("d9ea77826a7379732e4769744f6964826473686131");
    golden.push(0x54);
    golden.extend([0; 20]);
    let canonical = sha1.to_canonical().expect("a canonical sys.GitOid");
    assert_eq!(canonical.encode().expect("it encodes"), golden);
    let decoded = CanonicalValue::decode(&golden).expect("the golden vector decodes");
    assert_eq!(GitObjectId::from_opaque_raw(decoded.raw()), Some(sha1));

    for id in [
        GitObjectId::from_hex(SHA1_HEX).expect("a SHA-1 id"),
        GitObjectId::from_hex(&sha256_hex()).expect("a SHA-256 id"),
    ] {
        let bytes = id
            .to_canonical()
            .expect("canonical")
            .encode()
            .expect("it encodes");
        let decoded = CanonicalValue::decode(&bytes).expect("it decodes");
        assert_eq!(decoded.encode().expect("it re-encodes"), bytes);
        assert_eq!(GitObjectId::from_opaque_raw(decoded.raw()), Some(id));
    }
}

// Specified: ORNA-SYS-016; an id spells as the lowercase hexadecimal Git prints, and only that
// spelling names one.
#[test]
fn the_hexadecimal_spelling_round_trips_and_only_lowercase_digits_name_an_id() {
    for hex in [SHA1_HEX.to_owned(), sha256_hex()] {
        let id = GitObjectId::from_hex(&hex).expect("an id");
        assert_eq!(id.to_hex(), hex);
        assert_eq!(id.to_string(), hex);
        assert_eq!(id.as_bytes(), bytes_of(&hex).as_slice());
    }
    for rejected in [
        "".to_owned(),
        SHA1_HEX[..39].to_owned(),
        format!("{SHA1_HEX}0"),
        SHA1_HEX.to_uppercase(),
        format!("g{}", &SHA1_HEX[1..]),
        " ".repeat(40),
        // 40 bytes, but multi-byte characters rather than hexadecimal digits.
        "é".repeat(20),
        "c0".repeat(31),
        format!("{}0", sha256_hex()),
    ] {
        assert_eq!(
            GitObjectId::from_hex(&rejected),
            Err(SystemReferenceError::InvalidRelationKey),
            "{rejected:?}"
        );
    }
}

// Specified: ORNA-SYS-016; a tag-60023 value is a Git object id only when it names `sys.GitOid` with
// a representation that agrees with its algorithm.
#[test]
fn only_a_well_formed_sys_git_oid_reads_back_as_an_id() {
    let valid = opaque(GIT_OID_TYPE_NAME, git("sha1", vec![7; 20]));
    assert!(GitObjectId::from_opaque_raw(&valid).is_some());
    for rejected in [
        // The width disagrees with the algorithm.
        opaque(GIT_OID_TYPE_NAME, git("sha1", vec![7; 32])),
        opaque(GIT_OID_TYPE_NAME, git("sha256", vec![7; 20])),
        // An unknown algorithm.
        opaque(GIT_OID_TYPE_NAME, git("md5", vec![7; 16])),
        // Another identifier, even one of the same width.
        opaque("sys.RevisionId", OvbRaw::Bytes(vec![7; 32])),
        opaque("sys.Digest", git("sha256", vec![7; 32])),
        // The bare key representation is not the identifier value.
        git("sha1", vec![7; 20]),
        // Not a tag-60023 value at all.
        OvbRaw::Bytes(vec![7; 20]),
        OvbRaw::Text(SHA1_HEX.to_owned()),
        OvbRaw::Null,
    ] {
        assert_eq!(
            GitObjectId::from_opaque_raw(&rejected),
            None,
            "{rejected:?}"
        );
    }
    // The registry validates the same shapes, so none of the rejected tag-60023 forms is canonical.
    for malformed in [
        opaque(GIT_OID_TYPE_NAME, git("sha1", vec![7; 32])),
        opaque(GIT_OID_TYPE_NAME, git("sha256", vec![7; 20])),
        opaque(GIT_OID_TYPE_NAME, git("md5", vec![7; 16])),
    ] {
        assert!(CanonicalValue::new(malformed).is_err());
    }
}

// Specified: ORNA-SYS-016; ids compare by identity, and order by algorithm and then bytes.
#[test]
fn ids_compare_by_identity_and_order_by_algorithm_then_bytes() {
    let id = |hex: &str| GitObjectId::from_hex(hex).expect("an id");
    let low = id("0000000000000000000000000000000000000001");
    let middle = id(SHA1_HEX);
    let high = id("ffffffffffffffffffffffffffffffffffffffff");
    let sha256 = id(&"00".repeat(32));
    assert_eq!(middle, id(SHA1_HEX));
    assert_ne!(middle, high);
    assert!(low < middle && middle < high);
    // Every SHA-1 id sorts before every SHA-256 id, whatever its bytes.
    assert!(high < sha256);
    let mut sorted = vec![sha256.clone(), high.clone(), low.clone(), middle.clone()];
    sorted.sort();
    assert_eq!(sorted, [low.clone(), middle.clone(), high, sha256]);
    // For one algorithm the order is the order of the lowercase hexadecimal spellings.
    let mut by_hex = sorted[..3].to_vec();
    by_hex.sort_by_key(GitObjectId::to_hex);
    assert_eq!(by_hex, sorted[..3]);
    let distinct = HashSet::from([low.clone(), low, middle.clone(), middle]);
    assert_eq!(distinct.len(), 2);
}
