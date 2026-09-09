use std::io::{BufReader, Cursor};

use orna_client::{TerminalSessionDriver, TerminalSessionDriverError};
use orna_core::InvocationId;
use orna_protocol::{InputRequested, SessionClientFrame};

fn id(value: u8) -> InvocationId {
    InvocationId::from_bytes([value; 16])
}

fn request(request: u8, prompt: &str) -> InputRequested {
    InputRequested {
        root_invocation_id: id(1),
        call_stream: 7,
        request_invocation_id: id(request),
        prompt: prompt.to_owned(),
    }
}

#[test]
fn exact_replay_returns_the_retained_response_without_consuming_another_line() {
    let input = BufReader::new(Cursor::new(b"first\nsecond\n".to_vec()));
    let mut driver = TerminalSessionDriver::new(input, Vec::new(), id(1), 7).expect("driver");
    let first = request(2, "orna> ");

    let response = driver.respond_to(first.clone()).expect("first response");
    assert_eq!(
        response,
        SessionClientFrame::InputLine {
            root_invocation_id: id(1),
            call_stream: 7,
            request_invocation_id: id(2),
            line: "first".to_owned(),
        }
    );
    assert_eq!(
        driver.respond_to(first).expect("replayed response"),
        response,
    );
    assert!(matches!(
        driver.respond_to(request(2, "changed> ")),
        Err(TerminalSessionDriverError::ReplayedRequestMismatch)
    ));

    assert!(matches!(
        driver.respond_to(request(3, "next> ")).expect("next response"),
        SessionClientFrame::InputLine { line, .. } if line == "second"
    ));
    let (_, output) = driver.into_parts();
    assert_eq!(output, b"orna> next> ");
}
