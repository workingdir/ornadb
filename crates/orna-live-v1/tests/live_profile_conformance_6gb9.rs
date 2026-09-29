use futures::executor::block_on;
use orna_live_v1::{
    Error, Limits, LiveCredentialIssuer, LiveHost, LiveSessionAuthority,
    LiveTransport, SessionMetadata, TransportLimits, WireRequest,
};
use orna_protocol_v1::{
    Envelope, Message, PresentationContext, TargetKind,
};
use orna_security_v1::{
    BoundaryError, CredentialIssuer, Origin, OriginPolicy, SessionBoundary, SessionDeletionAdapter,
};
use orna_serving_v1::{Limits as ServingLimits, Serving};
use std::{collections::BTreeMap, env, fs, path::PathBuf};

#[derive(Clone, Debug, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Number(i64),
    String(String),
    Array(Vec<Json>),
    Object(BTreeMap<String, Json>),
}

struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Parser<'a> {
    fn parse(bytes: &'a [u8]) -> Result<Json, String> {
        let mut parser = Self { bytes, at: 0 };
        let value = parser.value()?;
        parser.space();
        if parser.at == parser.bytes.len() {
            Ok(value)
        } else {
            Err(format!("trailing JSON at byte {}", parser.at))
        }
    }

    fn value(&mut self) -> Result<Json, String> {
        self.space();
        match self.peek() {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => self.string().map(Json::String),
            Some(b't') => self.word(b"true", Json::Bool(true)),
            Some(b'f') => self.word(b"false", Json::Bool(false)),
            Some(b'n') => self.word(b"null", Json::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(format!("invalid JSON value at byte {}", self.at)),
        }
    }

    fn object(&mut self) -> Result<Json, String> {
        self.expect(b'{')?;
        self.space();
        let mut fields = BTreeMap::new();
        if self.peek() == Some(b'}') {
            self.at += 1;
            return Ok(Json::Object(fields));
        }
        loop {
            self.space();
            let key = self.string()?;
            self.space();
            self.expect(b':')?;
            let value = self.value()?;
            if fields.insert(key.clone(), value).is_some() {
                return Err(format!("duplicate JSON member {key}"));
            }
            self.space();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b'}') => {
                    self.at += 1;
                    break;
                }
                _ => return Err(format!("expected ',' or '}}' at byte {}", self.at)),
            }
        }
        Ok(Json::Object(fields))
    }

    fn array(&mut self) -> Result<Json, String> {
        self.expect(b'[')?;
        self.space();
        let mut values = Vec::new();
        if self.peek() == Some(b']') {
            self.at += 1;
            return Ok(Json::Array(values));
        }
        loop {
            values.push(self.value()?);
            self.space();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b']') => {
                    self.at += 1;
                    break;
                }
                _ => return Err(format!("expected ',' or ']' at byte {}", self.at)),
            }
        }
        Ok(Json::Array(values))
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut result = String::new();
        loop {
            match self.take() {
                Some(b'"') => return Ok(result),
                Some(b'\\') => match self.take() {
                    Some(b'"') => result.push('"'),
                    Some(b'\\') => result.push('\\'),
                    Some(b'/') => result.push('/'),
                    Some(b'b') => result.push('\u{0008}'),
                    Some(b'f') => result.push('\u{000c}'),
                    Some(b'n') => result.push('\n'),
                    Some(b'r') => result.push('\r'),
                    Some(b't') => result.push('\t'),
                    Some(b'u') => {
                        let end = self.at.checked_add(4).ok_or("unicode escape overflow")?;
                        let digits = self.bytes.get(self.at..end).ok_or("short unicode escape")?;
                        let hex = std::str::from_utf8(digits).map_err(|error| error.to_string())?;
                        let unit = u16::from_str_radix(hex, 16).map_err(|error| error.to_string())?;
                        let scalar = char::from_u32(u32::from(unit)).ok_or("invalid unicode scalar")?;
                        result.push(scalar);
                        self.at = end;
                    }
                    _ => return Err(format!("invalid JSON escape at byte {}", self.at)),
                },
                Some(byte) if byte < 0x20 => return Err("control byte in JSON string".into()),
                Some(byte) if byte < 0x80 => result.push(char::from(byte)),
                Some(_) => {
                    self.at -= 1;
                    let tail = std::str::from_utf8(&self.bytes[self.at..])
                        .map_err(|error| error.to_string())?;
                    let ch = tail.chars().next().ok_or("invalid UTF-8 JSON string")?;
                    self.at += ch.len_utf8();
                    result.push(ch);
                }
                None => return Err("unterminated JSON string".into()),
            }
        }
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.at += 1;
        }
        if matches!(self.peek(), Some(b'.' | b'e' | b'E')) {
            return Err("non-integer number in profile/session data".into());
        }
        let raw = std::str::from_utf8(&self.bytes[start..self.at]).map_err(|e| e.to_string())?;
        Ok(Json::Number(raw.parse().map_err(|e| format!("invalid integer: {e}"))?))
    }

    fn word(&mut self, word: &[u8], value: Json) -> Result<Json, String> {
        let end = self.at.checked_add(word.len()).ok_or("JSON offset overflow")?;
        if self.bytes.get(self.at..end) == Some(word) {
            self.at = end;
            Ok(value)
        } else {
            Err(format!("invalid JSON token at byte {}", self.at))
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), String> {
        if self.take() == Some(byte) {
            Ok(())
        } else {
            Err(format!("expected {:?} at byte {}", char::from(byte), self.at))
        }
    }
    fn peek(&self) -> Option<u8> { self.bytes.get(self.at).copied() }
    fn take(&mut self) -> Option<u8> { let byte = self.peek()?; self.at += 1; Some(byte) }
    fn space(&mut self) { while self.peek().is_some_and(|byte| byte.is_ascii_whitespace()) { self.at += 1; } }
}

fn object(value: &Json) -> &BTreeMap<String, Json> {
    match value { Json::Object(fields) => fields, other => panic!("expected object, got {other:?}") }
}
fn array(value: &Json) -> &[Json] {
    match value { Json::Array(values) => values, other => panic!("expected array, got {other:?}") }
}
fn string(value: &Json) -> &str {
    match value { Json::String(value) => value, other => panic!("expected string, got {other:?}") }
}
fn integer(value: &Json) -> i64 {
    match value { Json::Number(value) => *value, other => panic!("expected integer, got {other:?}") }
}
fn field<'a>(value: &'a Json, key: &str) -> &'a Json { object(value).get(key).unwrap_or_else(|| panic!("missing JSON field {key}")) }

fn reference_file(name: &str) -> String {
    let root = env::var_os("ORNA_REFERENCE_DIR").map(PathBuf::from).unwrap_or_else(|| {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        manifest.parent().expect("crate has parent").parent().expect("workspace has parent")
            .parent().expect("worktree has parent").join("../reference/Orna-1.0.0")
    });
    fs::read_to_string(root.join(name)).unwrap_or_else(|error| {
        panic!("could not read {name} under ORNA_REFERENCE_DIR/reference sibling: {error}")
    })
}
fn profile() -> Json { Parser::parse(reference_file("profiles/live-messages.json").as_bytes()).unwrap() }
fn schema() -> Json { Parser::parse(reference_file("profiles/session.schema.json").as_bytes()).unwrap() }

fn schema_check(instance: &Json, schema: &Json, path: &str) {
    let schema = object(schema);
    if let Some(Json::String(kind)) = schema.get("type") {
        match kind.as_str() {
            "object" => assert!(matches!(instance, Json::Object(_)), "{path} must be object"),
            "string" => assert!(matches!(instance, Json::String(_)), "{path} must be string"),
            "integer" => assert!(matches!(instance, Json::Number(_)), "{path} must be integer"),
            other => panic!("unhandled schema type {other}"),
        }
    }
    if let Some(expected) = schema.get("const") {
        assert_eq!(instance, expected, "{path} const mismatch");
    }
    if let Some(Json::String(pattern)) = schema.get("pattern") {
        let value = string(instance);
        let matches = match pattern.as_str() {
            "^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$" => valid_uuid(value),
            "^[A-Za-z0-9_-]{43}$" => value.len() == 43 && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
            "^/orna/live/[0-9a-fA-F-]{36}$" => value.strip_prefix("/orna/live/").is_some_and(valid_uuid),
            other => panic!("unhandled profile pattern {other}"),
        };
        assert!(matches, "{path} does not satisfy schema pattern {pattern}: {value}");
    }
    if let Some(Json::Number(minimum)) = schema.get("minimum") {
        assert!(integer(instance) >= *minimum, "{path} is below schema minimum {minimum}");
    }
    if let Some(Json::Array(required)) = schema.get("required") {
        let fields = object(instance);
        for name in required {
            let name = string(name);
            assert!(fields.contains_key(name), "{path} lacks required schema field {name}");
        }
    }
    if let Json::Object(fields) = instance {
        if schema.get("additionalProperties") == Some(&Json::Bool(false)) {
            let properties = object(schema.get("properties").expect("closed object has properties"));
            for name in fields.keys() {
                assert!(properties.contains_key(name), "{path} has unregistered field {name}");
            }
        }
        if let Some(Json::Object(properties)) = schema.get("properties") {
            for (name, value) in fields {
                if let Some(property_schema) = properties.get(name) {
                    schema_check(value, property_schema, &format!("{path}.{name}"));
                }
            }
        }
    }
}

fn selected_schema<'a>(schema: &'a Json, name: &str) -> &'a Json {
    field(field(schema, "$defs"), name)
}
fn valid_uuid(value: &str) -> bool {
    let parts = value.split('-').collect::<Vec<_>>();
    parts.len() == 5 && [8, 4, 4, 4, 12].into_iter().zip(parts).all(|(length, part)| {
        part.len() == length && part.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

#[test]
fn profile_registry_is_complete_and_matches_the_protocol_envelope() {
    let profile = profile();
    assert_eq!(string(field(&profile, "profile")), "orna.present.v1");
    assert_eq!(string(field(&profile, "value_profile")), "OVB-1");
    let envelope = field(&profile, "envelope");
    let required = object(field(envelope, "required"));
    assert_eq!(required.keys().map(String::as_str).collect::<Vec<_>>(), ["0", "1", "2", "3", "4"]);
    assert_eq!(string(field(envelope, "unknown_keys")), "reject");
    let extensions = field(&profile, "body_extensions");
    assert_eq!(array(field(extensions, "optional_unknown_range")), &[Json::Number(0), Json::Number(32767)]);
    assert_eq!(array(field(extensions, "mandatory_unknown_range")), &[Json::Number(32768), Json::Number(65535)]);
    assert_eq!(field(extensions, "fingerprint_includes_all_fields"), &Json::Bool(true));
    let definitions = field(&profile, "definitions");
    assert_eq!(string(field(definitions, "bytes16")), "A CBOR byte string with exactly 16 bytes; never text.");
    assert_eq!(string(field(definitions, "bytes32")), "A CBOR byte string with exactly 32 bytes.");
    assert_eq!(string(field(definitions, "UInt")), "An untagged CBOR integer >= 0; Bool is not an integer.");

    let expected = [
        (0, "client", "subscribe", "required", "null", &["0", "1"][..], &[][..]),
        (1, "client", "unsubscribe", "required", "required", &[][..], &[][..]),
        (2, "client", "resync", "required", "required", &[][..], &[][..]),
        (3, "client", "event", "required", "required", &["0", "1", "2", "3"][..], &[][..]),
        (4, "client", "eval", "required", "null", &["0", "1", "2", "3"][..], &[][..]),
        (5, "client", "watch", "required", "null", &["0", "1", "2"][..], &["3"][..]),
        (6, "client", "cancel", "required", "null", &["0", "1"][..], &[][..]),
        (7, "client", "request_status", "required", "null", &["0", "1"][..], &[][..]),
        (16, "host", "snapshot", "optional", "required", &["0", "1", "2"][..], &[][..]),
        (17, "host", "delta", "null", "required", &["0", "1", "2", "3"][..], &[][..]),
        (18, "host", "result", "required", "null", &["0", "1", "2", "3"][..], &[][..]),
        (19, "host", "diagnostic", "optional", "optional", &["0"][..], &["1"][..]),
        (20, "host", "request_status_result", "required", "null", &["0", "1", "2", "3"][..], &[][..]),
    ];
    let messages = array(field(&profile, "messages"));
    assert_eq!(messages.len(), expected.len(), "registry message count changed");
    for (row, expected) in messages.iter().zip(expected) {
        assert_eq!(integer(field(row, "code")), expected.0);
        assert_eq!(string(field(row, "direction")), expected.1);
        assert_eq!(string(field(row, "name")), expected.2);
        assert_eq!(string(field(row, "request")), expected.3);
        assert_eq!(string(field(row, "watch")), expected.4);
        for (field_name, expected_keys) in [("required", expected.5), ("optional", expected.6)] {
            let keys = object(field(row, field_name)).keys().map(String::as_str).collect::<Vec<_>>();
            assert_eq!(keys, expected_keys, "{} {field_name} body shape", expected.2);
        }
    }
}

#[test]
fn codec_accepts_profiled_client_control_envelopes_and_rejects_bad_identity_shape() {
    let profile = profile();
    let limits = Limits::default().protocol;
    let presentation = PresentationContext {
        locale: "en-GB".into(), timezone: None, width: Some(80), theme: "terminal/dark".into(), supported_kinds: vec!["value".into()],
    };
    let envelopes = [
        Envelope { request: Some([1; 16]), watch: None, message: Message::Subscribe { resource: [2; 16], presentation }, extensions: BTreeMap::new() },
        Envelope { request: Some([3; 16]), watch: Some([4; 16]), message: Message::Unsubscribe, extensions: BTreeMap::new() },
        Envelope { request: Some([5; 16]), watch: Some([6; 16]), message: Message::Resync, extensions: BTreeMap::new() },
        Envelope { request: Some([7; 16]), watch: None, message: Message::Cancel { target_kind: TargetKind::Watch, target: [8; 16] }, extensions: BTreeMap::new() },
        Envelope { request: Some([9; 16]), watch: None, message: Message::RequestStatus { target: [10; 16], fingerprint: [11; 32] }, extensions: BTreeMap::new() },
    ];
    for envelope in envelopes {
        let code = envelope.message.code();
        let profile_row = array(field(&profile, "messages")).iter().find(|row| integer(field(row, "code")) == code as i64).expect("codec code is in live-messages registry");
        assert_eq!(string(field(profile_row, "direction")), "client");
        let request_rule = string(field(profile_row, "request"));
        let watch_rule = string(field(profile_row, "watch"));
        assert_eq!(request_rule, "required");
        assert_eq!(watch_rule == "required", envelope.watch.is_some());
        let bytes = envelope.encode(limits).expect("valid profile client shape encodes");
        assert_eq!(Envelope::decode(&bytes, limits).unwrap(), envelope);
    }
    let wrong_request_rule = Envelope {
        request: None, watch: None, message: Message::Subscribe { resource: [2; 16], presentation: PresentationContext { locale: "en".into(), timezone: None, width: None, theme: "web/default".into(), supported_kinds: vec![] } }, extensions: BTreeMap::new(),
    };
    assert!(wrong_request_rule.encode(limits).is_err());
}

struct Authority { calls: usize }
impl LiveSessionAuthority for Authority {
    fn create_session(&mut self, database: [u8; 16], now: u64) -> Result<SessionMetadata, Error> {
        self.calls += 1;
        Ok(SessionMetadata { session: [0x11; 16], database, runtime: [0x22; 16], expires_at: now + 60_000, subscribe: subscribe_bytes() })
    }
}
struct Issuer { next: u8, last: Option<[u8; 32]> }
impl CredentialIssuer for Issuer {
    fn issue_credential(&mut self) -> Result<[u8; 32], BoundaryError> {
        let token = [self.next; 32];
        self.next += 1;
        self.last = Some(token);
        Ok(token)
    }
}
impl LiveCredentialIssuer for Issuer { fn last_issued(&self) -> Option<[u8; 32]> { self.last } }
struct Delete;
impl SessionDeletionAdapter for Delete {
    type Error = ();
    fn delete(&mut self, _: orna_security_v1::SessionId) -> Result<(), Self::Error> { Ok(()) }
}
fn subscribe_bytes() -> Vec<u8> {
    Envelope { request: Some([0x33; 16]), watch: None, message: Message::Subscribe { resource: [0x44; 16], presentation: PresentationContext { locale: "en-GB".into(), timezone: None, width: None, theme: "web/default".into(), supported_kinds: vec![] } }, extensions: BTreeMap::new() }
        .encode(Limits::default().protocol).unwrap()
}
fn host() -> LiveHost {
    let origin = Origin::parse("https://app.example").unwrap();
    LiveHost::new(Limits::default(), SessionBoundary::new(OriginPolicy::new([origin], []), 10), Serving::new(ServingLimits::default()).unwrap()).unwrap()
}
fn wire(body: &str) -> WireRequest {
    WireRequest { method: "POST".into(), path: "/orna/session".into(), headers: vec![("origin".into(), "https://app.example".into()), ("host".into(), "app.example".into()), ("content-type".into(), "application/json".into())], body: body.as_bytes().to_vec() }
}
fn uuid(byte: u8) -> String {
    let raw = format!("{byte:02x}").repeat(16);
    format!("{}-{}-{}-{}-{}", &raw[..8], &raw[8..12], &raw[12..16], &raw[16..20], &raw[20..])
}
fn token(response: &orna_live_v1::WireResponse) -> String {
    let parsed = Parser::parse(&response.body).unwrap();
    string(field(&parsed, "resume_token")).to_owned()
}

#[test]
fn create_response_satisfies_the_checked_in_session_response_schema() {
    let schema = schema();
    let response_schema = selected_schema(&schema, "session_response");
    let create_schema = selected_schema(&schema, "create_request");
    let request = Parser::parse(format!(r#"{{"database":"{}","protocol":"orna.present.v1"}}"#, uuid(0x55)).as_bytes()).unwrap();
    schema_check(&request, create_schema, "create request");

    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut authority = Authority { calls: 0 };
    let mut issuer = Issuer { next: 1, last: None };
    let response = block_on(transport.handle(wire(&format!(r#"{{"database":"{}","protocol":"orna.present.v1"}}"#, uuid(0x55))), 10, &mut authority, &mut issuer, &mut Delete));
    assert_eq!(response.status, 201);
    let body = Parser::parse(&response.body).unwrap();
    schema_check(&body, response_schema, "create response");
    assert_eq!(field(&body, "websocket_path"), &Json::String(format!("/orna/live/{}", string(field(&body, "session")))));
    let limits = field(&body, "limits");
    assert!(integer(field(limits, "request_retention_ms")) >= integer(field(&body, "lease_ms")));
    let cookie = response.headers.iter().find(|(name, _)| name == "set-cookie").expect("session response sets a scoped cookie");
    assert!(cookie.1.contains("HttpOnly") && cookie.1.contains("SameSite=Strict"));
}

#[test]
fn resume_response_obeys_schema_and_rotates_the_profile_token() {
    let schema = schema();
    let resume_schema = selected_schema(&schema, "resume_request");
    let response_schema = selected_schema(&schema, "session_response");
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut authority = Authority { calls: 0 };
    let mut issuer = Issuer { next: 1, last: None };
    let create = block_on(transport.handle(wire(&format!(r#"{{"database":"{}","protocol":"orna.present.v1"}}"#, uuid(0x55))), 10, &mut authority, &mut issuer, &mut Delete));
    let before = Parser::parse(&create.body).unwrap();
    let old_token = token(&create);
    let resume_request = Parser::parse(format!(r#"{{"resume_token":"{old_token}","protocol":"orna.present.v1"}}"#).as_bytes()).unwrap();
    schema_check(&resume_request, resume_schema, "resume request");
    let path = format!("/orna/session/{}/resume", string(field(&before, "session")));
    let mut noncanonical_token = old_token.clone();
    noncanonical_token.pop();
    noncanonical_token.push('R');
    let rejected = block_on(transport.handle(WireRequest { path: path.clone(), ..wire(&format!(r#"{{"resume_token":"{noncanonical_token}","protocol":"orna.present.v1"}}"#)) }, 11, &mut authority, &mut issuer, &mut Delete));
    assert_eq!(rejected.status, 400, "non-zero unused Base64url bits are rejected");
    let mismatched_path = format!("/orna/session/{}/resume", uuid(0x66));
    let rejected = block_on(transport.handle(WireRequest { path: mismatched_path, ..wire(&format!(r#"{{"resume_token":"{old_token}","protocol":"orna.present.v1"}}"#)) }, 11, &mut authority, &mut issuer, &mut Delete));
    assert_eq!(rejected.status, 410, "resume credentials cannot be moved to another session path");
    let resumed = block_on(transport.handle(WireRequest { path, ..wire(&format!(r#"{{"resume_token":"{old_token}","protocol":"orna.present.v1"}}"#)) }, 11, &mut authority, &mut issuer, &mut Delete));
    assert_eq!(resumed.status, 200);
    let after = Parser::parse(&resumed.body).unwrap();
    schema_check(&after, response_schema, "resume response");
    assert_eq!(field(&after, "session"), field(&before, "session"));
    assert_eq!(field(&after, "database"), field(&before, "database"));
    assert_ne!(field(&after, "resume_token"), field(&before, "resume_token"));
    assert_eq!(field(&after, "websocket_path"), field(&before, "websocket_path"));
}

#[test]
fn create_profile_rejects_duplicate_unknown_and_wrongly_typed_members_before_admission() {
    let schema = schema();
    let create_schema = selected_schema(&schema, "create_request");
    let valid = Parser::parse(format!(r#"{{"database":"{}","protocol":"orna.present.v1"}}"#, uuid(0x55)).as_bytes()).unwrap();
    schema_check(&valid, create_schema, "valid create request");

    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut authority = Authority { calls: 0 };
    let mut issuer = Issuer { next: 1, last: None };
    for body in [
        format!(r#"{{"database":"{}","database":"{}","protocol":"orna.present.v1"}}"#, uuid(0x55), uuid(0x55)),
        format!(r#"{{"database":"{}","protocol":"orna.present.v1","extra":"no-v1-extension"}}"#, uuid(0x55)),
        r#"{"database":7,"protocol":"orna.present.v1"}"#.to_owned(),
    ] {
        let response = block_on(transport.handle(wire(&body), 10, &mut authority, &mut issuer, &mut Delete));
        assert_eq!(response.status, 400);
    }
    assert_eq!(authority.calls, 0, "invalid profile requests are rejected before admission");
    assert_eq!(issuer.last, None, "invalid profile requests do not issue session tokens");
}
