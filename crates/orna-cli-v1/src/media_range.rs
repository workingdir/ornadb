//! Single byte-range resolution for explicit media responses (ORNA-APP-014).
//!
//! Follows RFC 9110 section 14: only the `bytes` unit is honoured, a single
//! range is served as `206`, an unsatisfiable range is `416`, and anything the
//! server does not understand is ignored so the full representation is served.
//! Multiple ranges are ignored rather than assembled as multipart responses.

/// Outcome of evaluating a `Range` header against a representation length.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RangeOutcome {
    /// Serve the whole representation with `200`.
    Full,
    /// Serve the inclusive byte span `start..=end` with `206`.
    Partial { start: u64, end: u64 },
    /// No requested byte exists; respond `416`.
    Unsatisfiable,
}

/// Resolves an optional `Range` header value against `length`.
#[cfg_attr(not(test), allow(dead_code))]
pub fn resolve_range(header: Option<&str>, length: u64) -> RangeOutcome {
    let Some(header) = header else {
        return RangeOutcome::Full;
    };
    let Some(spec) = header.trim().strip_prefix("bytes=") else {
        return RangeOutcome::Full;
    };
    if spec.contains(',') {
        return RangeOutcome::Full;
    }
    let Some((first, last)) = spec.trim().split_once('-') else {
        return RangeOutcome::Full;
    };
    let (first, last) = (first.trim(), last.trim());
    match (first.is_empty(), last.is_empty()) {
        // `bytes=-N`: the final N bytes. A zero suffix or an empty
        // representation has no satisfiable byte.
        (true, false) => {
            let Some(suffix) = parse_digits(last) else {
                return RangeOutcome::Full;
            };
            if suffix == 0 || length == 0 {
                return RangeOutcome::Unsatisfiable;
            }
            RangeOutcome::Partial {
                start: length.saturating_sub(suffix),
                end: length - 1,
            }
        }
        // `bytes=N-`: from N to the end.
        (false, true) => {
            let Some(start) = parse_digits(first) else {
                return RangeOutcome::Full;
            };
            if start >= length {
                return RangeOutcome::Unsatisfiable;
            }
            RangeOutcome::Partial {
                start,
                end: length - 1,
            }
        }
        // `bytes=N-M`: a closed range. An inverted range is invalid and is
        // ignored, as RFC 9110 allows.
        (false, false) => {
            let (Some(start), Some(last)) = (parse_digits(first), parse_digits(last)) else {
                return RangeOutcome::Full;
            };
            if start > last {
                return RangeOutcome::Full;
            }
            if start >= length {
                return RangeOutcome::Unsatisfiable;
            }
            RangeOutcome::Partial {
                start,
                end: last.min(length - 1),
            }
        }
        (true, true) => RangeOutcome::Full,
    }
}

/// HTTP method of an explicit media request. `HEAD` carries the same status
/// and headers as `GET` but never a body (RFC 9110 section 9.3.2).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaMethod {
    Get,
    Head,
}

/// Status, headers and body decision for one explicit media response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaResponsePlan {
    pub status: u16,
    /// Value for the `Content-Range` header, when the status carries one.
    /// `bytes */length` accompanies `416` (RFC 9110 section 15.5.17).
    pub content_range: Option<String>,
    /// Value for the `Content-Length` header: the bytes this response carries.
    pub content_length: u64,
    /// Whether the response includes the selected bytes.
    pub send_body: bool,
}

/// Plans the response for `method` and an optional `Range` header against a
/// representation of `length` bytes.
#[cfg_attr(not(test), allow(dead_code))]
pub fn plan_media_response(method: MediaMethod, range: Option<&str>, length: u64) -> MediaResponsePlan {
    let send_body = method == MediaMethod::Get;
    match resolve_range(range, length) {
        RangeOutcome::Full => MediaResponsePlan {
            status: 200,
            content_range: None,
            content_length: length,
            send_body,
        },
        RangeOutcome::Partial { start, end } => MediaResponsePlan {
            status: 206,
            content_range: Some(format!("bytes {start}-{end}/{length}")),
            content_length: end - start + 1,
            send_body,
        },
        RangeOutcome::Unsatisfiable => MediaResponsePlan {
            status: 416,
            content_range: Some(format!("bytes */{length}")),
            content_length: 0,
            send_body: false,
        },
    }
}

/// Failure while producing an explicit media body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MediaReplyError<E> {
    /// The byte reader reported a failure for the requested span.
    Read(E),
    /// The reader returned other than the planned `content_length` bytes.
    ShortRead { expected: u64, actual: u64 },
}

/// A planned explicit media response with the bytes it carries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaReply {
    pub plan: MediaResponsePlan,
    pub body: Vec<u8>,
}

/// Plans one explicit media response and reads only the bytes it carries.
///
/// `read_span` receives the inclusive `start..=end` span of the representation
/// and is never called for `HEAD`, for `416`, or for an empty representation.
/// A `206` therefore loads only its requested span, never the full payload.
#[cfg_attr(not(test), allow(dead_code))]
pub fn media_reply<E>(
    method: MediaMethod,
    range: Option<&str>,
    length: u64,
    read_span: impl FnOnce(u64, u64) -> Result<Vec<u8>, E>,
) -> Result<MediaReply, MediaReplyError<E>> {
    let plan = plan_media_response(method, range, length);
    let span = match resolve_range(range, length) {
        RangeOutcome::Full if length > 0 => Some((0, length - 1)),
        RangeOutcome::Full => None,
        RangeOutcome::Partial { start, end } => Some((start, end)),
        RangeOutcome::Unsatisfiable => None,
    };
    let body = match span {
        Some((start, end)) if plan.send_body => {
            let body = read_span(start, end).map_err(MediaReplyError::Read)?;
            let actual = body.len() as u64;
            if actual != plan.content_length {
                return Err(MediaReplyError::ShortRead {
                    expected: plan.content_length,
                    actual,
                });
            }
            body
        }
        _ => Vec::new(),
    };
    Ok(MediaReply { plan, body })
}

fn parse_digits(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_or_foreign_unit_serves_full_representation() {
        assert_eq!(resolve_range(None, 10), RangeOutcome::Full);
        assert_eq!(resolve_range(Some("items=0-4"), 10), RangeOutcome::Full);
        assert_eq!(resolve_range(Some("bytes"), 10), RangeOutcome::Full);
    }

    #[test]
    fn closed_range_is_clamped_to_representation() {
        assert_eq!(
            resolve_range(Some("bytes=2-5"), 10),
            RangeOutcome::Partial { start: 2, end: 5 }
        );
        assert_eq!(
            resolve_range(Some("bytes=2-99"), 10),
            RangeOutcome::Partial { start: 2, end: 9 }
        );
    }

    #[test]
    fn open_ended_and_suffix_ranges() {
        assert_eq!(
            resolve_range(Some("bytes=4-"), 10),
            RangeOutcome::Partial { start: 4, end: 9 }
        );
        assert_eq!(
            resolve_range(Some("bytes=-3"), 10),
            RangeOutcome::Partial { start: 7, end: 9 }
        );
        assert_eq!(
            resolve_range(Some("bytes=-30"), 10),
            RangeOutcome::Partial { start: 0, end: 9 }
        );
    }

    #[test]
    fn start_beyond_end_is_unsatisfiable() {
        assert_eq!(resolve_range(Some("bytes=10-"), 10), RangeOutcome::Unsatisfiable);
        assert_eq!(resolve_range(Some("bytes=10-12"), 10), RangeOutcome::Unsatisfiable);
        assert_eq!(resolve_range(Some("bytes=-0"), 10), RangeOutcome::Unsatisfiable);
    }

    #[test]
    fn empty_representation_has_no_satisfiable_suffix() {
        assert_eq!(resolve_range(Some("bytes=-5"), 0), RangeOutcome::Unsatisfiable);
        assert_eq!(resolve_range(Some("bytes=0-"), 0), RangeOutcome::Unsatisfiable);
    }

    #[test]
    fn invalid_or_multiple_ranges_are_ignored() {
        assert_eq!(resolve_range(Some("bytes=5-2"), 10), RangeOutcome::Full);
        assert_eq!(resolve_range(Some("bytes=0-1,4-5"), 10), RangeOutcome::Full);
        assert_eq!(resolve_range(Some("bytes=a-b"), 10), RangeOutcome::Full);
        assert_eq!(resolve_range(Some("bytes=-"), 10), RangeOutcome::Full);
    }

    #[test]
    fn full_get_and_head_share_headers_but_only_get_sends_body() {
        assert_eq!(
            plan_media_response(MediaMethod::Get, None, 10),
            MediaResponsePlan {
                status: 200,
                content_range: None,
                content_length: 10,
                send_body: true,
            }
        );
        let head = plan_media_response(MediaMethod::Head, None, 10);
        assert_eq!(head.status, 200);
        assert_eq!(head.content_length, 10);
        assert!(!head.send_body);
    }

    #[test]
    fn partial_get_reads_only_the_requested_span() {
        let mut requested = Vec::new();
        let reply = media_reply(MediaMethod::Get, Some("bytes=2-4"), 10, |start, end| {
            requested.push((start, end));
            Ok::<_, ()>(vec![2, 3, 4])
        })
        .expect("partial read");
        assert_eq!(requested, vec![(2, 4)]);
        assert_eq!(reply.plan.status, 206);
        assert_eq!(reply.body, vec![2, 3, 4]);
    }

    #[test]
    fn full_get_reads_the_whole_representation_once() {
        let mut requested = Vec::new();
        let reply = media_reply(MediaMethod::Get, None, 4, |start, end| {
            requested.push((start, end));
            Ok::<_, ()>(vec![9; 4])
        })
        .expect("full read");
        assert_eq!(requested, vec![(0, 3)]);
        assert_eq!(reply.plan.status, 200);
        assert_eq!(reply.body.len(), 4);
    }

    #[test]
    fn head_and_unsatisfiable_never_read_bytes() {
        let mut calls = 0;
        let head = media_reply(MediaMethod::Head, Some("bytes=0-1"), 10, |_, _| {
            calls += 1;
            Ok::<_, ()>(vec![0; 2])
        })
        .expect("head plan");
        let unsatisfiable = media_reply(MediaMethod::Get, Some("bytes=50-60"), 10, |_, _| {
            calls += 1;
            Ok::<_, ()>(vec![0; 11])
        })
        .expect("416 plan");
        assert_eq!(calls, 0);
        assert_eq!(head.plan.status, 206);
        assert!(head.body.is_empty());
        assert_eq!(unsatisfiable.plan.status, 416);
        assert!(unsatisfiable.body.is_empty());
    }

    #[test]
    fn empty_representation_reads_nothing() {
        let reply = media_reply(MediaMethod::Get, None, 0, |_, _| {
            Ok::<Vec<u8>, ()>(panic!("empty representation must not read"))
        })
        .expect("empty plan");
        assert_eq!(reply.plan.status, 200);
        assert!(reply.body.is_empty());
    }

    #[test]
    fn short_read_is_rejected_not_served() {
        let error = media_reply(MediaMethod::Get, Some("bytes=0-3"), 10, |_, _| {
            Ok::<_, ()>(vec![1, 2])
        })
        .expect_err("short read");
        assert_eq!(
            error,
            MediaReplyError::ShortRead {
                expected: 4,
                actual: 2
            }
        );
    }

    #[test]
    fn reader_failure_is_surfaced() {
        let error = media_reply(MediaMethod::Get, Some("bytes=0-0"), 10, |_, _| {
            Err::<Vec<u8>, _>("verification failed")
        })
        .expect_err("reader failure");
        assert_eq!(error, MediaReplyError::Read("verification failed"));
    }

    #[test]
    fn partial_response_carries_content_range_and_span_length() {
        assert_eq!(
            plan_media_response(MediaMethod::Get, Some("bytes=2-5"), 10),
            MediaResponsePlan {
                status: 206,
                content_range: Some("bytes 2-5/10".to_owned()),
                content_length: 4,
                send_body: true,
            }
        );
        let head = plan_media_response(MediaMethod::Head, Some("bytes=-3"), 10);
        assert_eq!(head.status, 206);
        assert_eq!(head.content_range.as_deref(), Some("bytes 7-9/10"));
        assert_eq!(head.content_length, 3);
        assert!(!head.send_body);
    }

    #[test]
    fn unsatisfiable_range_is_416_with_no_body_and_unsatisfied_content_range() {
        assert_eq!(
            plan_media_response(MediaMethod::Get, Some("bytes=10-"), 10),
            MediaResponsePlan {
                status: 416,
                content_range: Some("bytes */10".to_owned()),
                content_length: 0,
                send_body: false,
            }
        );
    }
}
