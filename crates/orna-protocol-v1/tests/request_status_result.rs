use orna_foundation_v1::CanonicalValue;
use orna_protocol_v1::{Envelope, Error, Limits, Message, RequestState, ResultBody, ResultStatus};
use std::collections::BTreeMap;

fn result_body() -> ResultBody {
    let response = Envelope {
        request: Some([9; 16]),
        watch: None,
        message: Message::Result {
            status: ResultStatus::Failure,
            value: None,
            fingerprint: [2; 32],
            diagnostic: None,
        },
        extensions: BTreeMap::new(),
    };
    ResultBody::from_result(&response, Limits::default()).unwrap()
}

fn status_message(
    state: RequestState,
    fingerprint: Option<[u8; 32]>,
    result: Option<ResultBody>,
) -> Envelope {
    Envelope {
        request: Some([8; 16]),
        watch: None,
        message: Message::RequestStatusResult {
            target: [1; 16],
            state,
            fingerprint,
            result,
        },
        extensions: BTreeMap::new(),
    }
}

fn raw_status(state: u8, fingerprint: Option<&[u8; 32]>, result: Option<&[u8]>) -> Vec<u8> {
    let mut bytes = vec![0xa5, 0x00, 0x01, 0x01, 0x14, 0x02, 0x50];
    bytes.extend([1; 16]);
    bytes.extend([0x03, 0xf6, 0x04, 0xa4, 0x00, 0x50]);
    bytes.extend([1; 16]);
    bytes.extend([0x01, state, 0x02]);
    match fingerprint {
        Some(fingerprint) => {
            bytes.extend([0x58, 0x20]);
            bytes.extend(fingerprint);
        }
        None => bytes.push(0xf6),
    }
    bytes.push(0x03);
    match result {
        Some(result) => bytes.extend_from_slice(result),
        None => bytes.push(0xf6),
    }
    bytes
}

#[test]
fn request_status_result_round_trips_every_state_and_valid_optional_shapes() {
    let valid_result = result_body();
    let cases = [
        (RequestState::Unknown, None, None),
        (RequestState::Reserved, Some([2; 32]), None),
        (RequestState::Running, Some([2; 32]), None),
        (
            RequestState::Terminal,
            Some([2; 32]),
            Some(valid_result.clone()),
        ),
        // Terminal results may retain the fingerprint while pruning the body.
        (RequestState::Terminal, Some([2; 32]), None),
        // Fingerprint and result retention are independent for orphaned work:
        // cover retained, result-pruned, fingerprint-pruned, and fully-pruned.
        (
            RequestState::Orphaned,
            Some([2; 32]),
            Some(valid_result.clone()),
        ),
        (RequestState::Orphaned, Some([2; 32]), None),
        (RequestState::Orphaned, None, Some(valid_result.clone())),
        (RequestState::Orphaned, None, None),
    ];

    for (state, fingerprint, result) in cases {
        let encoded = status_message(state, fingerprint, result.clone())
            .encode(Limits::default())
            .unwrap();
        let decoded = Envelope::decode(&encoded, Limits::default()).unwrap();
        assert_eq!(decoded, status_message(state, fingerprint, result));
    }
}

#[test]
fn request_status_result_rejects_unknown_state_code() {
    assert_eq!(
        Envelope::decode(&raw_status(5, None, None), Limits::default()),
        Err(Error::InvalidValue)
    );
}

#[test]
fn request_status_result_rejects_malformed_result_body() {
    let malformed = [0xa1, 0x00, 0x01];
    assert_eq!(
        Envelope::decode(
            &raw_status(4, Some(&[2; 32]), Some(&malformed)),
            Limits::default(),
        ),
        Err(Error::InvalidValue)
    );
}

#[test]
fn request_status_result_rejects_noncanonical_result_body_encoding() {
    // The inner Result status is the canonical unsigned integer 1 encoded as
    // the noncanonical two-byte form 0x18 0x01.
    let noncanonical = [
        0xa4, 0x00, 0x18, 0x01, 0x01, 0xf6, 0x02, 0x58, 0x20, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
        2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 0x03, 0xf6,
    ];
    assert_eq!(
        Envelope::decode(
            &raw_status(4, Some(&[2; 32]), Some(&noncanonical)),
            Limits::default(),
        ),
        Err(Error::NonCanonical)
    );
}

#[test]
fn request_status_result_rejects_noncanonical_outer_state_encoding() {
    let mut encoded = raw_status(1, None, None);
    // Locate the state value after body key 1 and expand uint(1) to 0x18 0x01.
    let state = encoded
        .windows(3)
        .position(|bytes| bytes == [0x01, 0x01, 0x02])
        .map(|index| index + 1)
        .unwrap();
    encoded.splice(state..=state, [0x18, 0x01]);
    assert_eq!(
        Envelope::decode(&encoded, Limits::default()),
        Err(Error::NonCanonical)
    );
}

#[test]
fn valid_result_body_preserves_typed_unit_when_present() {
    let response = Envelope {
        request: Some([9; 16]),
        watch: None,
        message: Message::Result {
            status: ResultStatus::Success,
            value: Some(CanonicalValue::unit()),
            fingerprint: [2; 32],
            diagnostic: None,
        },
        extensions: BTreeMap::new(),
    };
    let body = ResultBody::from_result(&response, Limits::default()).unwrap();
    let status = status_message(RequestState::Terminal, Some([2; 32]), Some(body));
    let encoded = status.encode(Limits::default()).unwrap();
    assert_eq!(
        Envelope::decode(&encoded, Limits::default()).unwrap(),
        status
    );
}
