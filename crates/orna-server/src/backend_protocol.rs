use std::{
    io::{self, BufRead, Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    str,
};

use tokio_postgres::{Config, config::Host};

use super::{BackendShellError, interrupt_requested};

const PROTOCOL_VERSION_3: u32 = 196_608;
const CANCEL_REQUEST_CODE: u32 = 80_877_102;
const MAXIMUM_MESSAGE_LENGTH: usize = 64 * 1024 * 1024;
const COPY_PROMPT: &[u8] = b"copy=> ";
const COPY_CANCELLED: &str = "Orna COPY input cancelled";
const COPY_EOF: &str = "Orna COPY input ended before \\.";

type ShellResult<T> = Result<T, BackendShellError>;

pub(super) struct BackendSession {
    stream: UnixStream,
    socket_path: PathBuf,
    backend_pid: u32,
    secret_key: u32,
    active_query: bool,
    cancel_sent: bool,
    terminated: bool,
}

impl BackendSession {
    pub(super) fn connect(config: &Config) -> ShellResult<Self> {
        let socket_path = fixed_socket_path(config)?;
        let user = config
            .get_user()
            .filter(|user| *user == "orna_kernel")
            .ok_or(BackendShellError::AttachFailed)?;
        let database = config
            .get_dbname()
            .filter(|database| *database == "orna")
            .ok_or(BackendShellError::AttachFailed)?;
        let stream =
            UnixStream::connect(&socket_path).map_err(|_| BackendShellError::AttachFailed)?;
        let mut session = Self {
            stream,
            socket_path,
            backend_pid: 0,
            secret_key: 0,
            active_query: false,
            cancel_sent: false,
            terminated: false,
        };
        session.send_startup(user, database)?;
        session.receive_startup()?;
        Ok(session)
    }

    pub(super) fn execute(
        &mut self,
        query: &str,
        input: &mut impl BufRead,
        output: &mut impl Write,
    ) -> ShellResult<()> {
        if query.as_bytes().contains(&0) {
            return Err(BackendShellError::SessionFailed);
        }
        let mut payload = Vec::with_capacity(query.len() + 1);
        payload.extend_from_slice(query.as_bytes());
        payload.push(0);
        self.send_message(b'Q', &payload)?;
        self.active_query = true;
        self.cancel_sent = false;

        let mut columns = None;
        let mut copy_out = false;
        let mut saw_cancel_error = false;
        let mut fail_after_ready = false;
        loop {
            let message = self.read_message()?;
            match message.kind {
                b'T' if !copy_out => {
                    let names = parse_row_description(&message.payload)?;
                    render_fields(names.iter().map(String::as_str).map(Some), output)?;
                    columns = Some(names.len());
                }
                b'D' if !copy_out => {
                    let expected = columns.ok_or(BackendShellError::SessionFailed)?;
                    let values = parse_data_row(&message.payload, expected)?;
                    render_fields(values.iter().map(Option::as_deref), output)?;
                }
                b'C' if !copy_out => {
                    render_command_tag(&message.payload, output)?;
                    columns = None;
                }
                b'I' if !copy_out && message.payload.is_empty() => {
                    output
                        .write_all(b"COMMAND\n")
                        .map_err(|_| BackendShellError::SessionFailed)?;
                    columns = None;
                }
                b'E' => {
                    render_database_message("ERROR", &message.payload, output)?;
                    saw_cancel_error = self.cancel_sent;
                    columns = None;
                    copy_out = false;
                }
                b'N' => render_database_message("NOTICE", &message.payload, output)?,
                b'S' => require_parameter_status(&message.payload)?,
                b'G' if !copy_out && columns.is_none() => {
                    require_text_copy(&message.payload)?;
                    match self.copy_input(input, output)? {
                        CopyInputEnd::Completed | CopyInputEnd::Cancelled => {}
                        CopyInputEnd::Incomplete => fail_after_ready = true,
                    }
                }
                b'H' if !copy_out && columns.is_none() => {
                    require_text_copy(&message.payload)?;
                    copy_out = true;
                }
                b'd' if copy_out => {
                    write_escaped_bytes(&message.payload, output)?;
                    output
                        .write_all(b"\n")
                        .map_err(|_| BackendShellError::SessionFailed)?;
                }
                b'c' if copy_out && message.payload.is_empty() => copy_out = false,
                b'W' => return self.fail_session(),
                b'Z' if !copy_out => {
                    let status = require_ready(&message.payload)?;
                    self.active_query = false;
                    render_transaction_status(status, output)?;
                    output
                        .flush()
                        .map_err(|_| BackendShellError::SessionFailed)?;
                    if fail_after_ready || (self.cancel_sent && !saw_cancel_error) {
                        return Err(BackendShellError::SessionFailed);
                    }
                    return Ok(());
                }
                _ => return self.fail_session(),
            }
        }
    }

    pub(super) fn terminate(&mut self) -> ShellResult<()> {
        if !self.terminated {
            self.send_message(b'X', &[])?;
            self.terminated = true;
        }
        Ok(())
    }

    fn send_startup(&mut self, user: &str, database: &str) -> ShellResult<()> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&PROTOCOL_VERSION_3.to_be_bytes());
        for (name, value) in [
            ("user", user),
            ("database", database),
            ("client_encoding", "UTF8"),
            ("application_name", "orna backend-shell"),
        ] {
            payload.extend_from_slice(name.as_bytes());
            payload.push(0);
            payload.extend_from_slice(value.as_bytes());
            payload.push(0);
        }
        payload.push(0);
        let length = payload
            .len()
            .checked_add(4)
            .and_then(|length| u32::try_from(length).ok())
            .ok_or(BackendShellError::AttachFailed)?;
        self.stream
            .write_all(&length.to_be_bytes())
            .and_then(|()| self.stream.write_all(&payload))
            .map_err(|_| BackendShellError::AttachFailed)
    }

    fn receive_startup(&mut self) -> ShellResult<()> {
        let mut authenticated = false;
        let mut key_received = false;
        loop {
            let message = self
                .read_message_inner(false)
                .map_err(|_| BackendShellError::AttachFailed)?;
            match message.kind {
                b'R' if !authenticated
                    && !key_received
                    && message.payload == 0_u32.to_be_bytes() =>
                {
                    authenticated = true;
                }
                b'S' if authenticated && !key_received => {
                    require_parameter_status(&message.payload)
                        .map_err(|_| BackendShellError::AttachFailed)?
                }
                b'K' if authenticated && !key_received && message.payload.len() == 8 => {
                    self.backend_pid = u32::from_be_bytes(
                        message.payload[..4]
                            .try_into()
                            .map_err(|_| BackendShellError::AttachFailed)?,
                    );
                    self.secret_key = u32::from_be_bytes(
                        message.payload[4..]
                            .try_into()
                            .map_err(|_| BackendShellError::AttachFailed)?,
                    );
                    if self.backend_pid <= 1 {
                        return Err(BackendShellError::AttachFailed);
                    }
                    key_received = true;
                }
                b'Z' if authenticated && key_received => {
                    if require_ready(&message.payload)
                        .map_err(|_| BackendShellError::AttachFailed)?
                        != b'I'
                    {
                        return Err(BackendShellError::AttachFailed);
                    }
                    return Ok(());
                }
                _ => return Err(BackendShellError::AttachFailed),
            }
        }
    }

    fn copy_input(
        &mut self,
        input: &mut impl BufRead,
        output: &mut impl Write,
    ) -> ShellResult<CopyInputEnd> {
        let mut line = String::new();
        loop {
            output
                .write_all(COPY_PROMPT)
                .and_then(|()| output.flush())
                .map_err(|_| BackendShellError::SessionFailed)?;
            line.clear();
            let length = match input.read_line(&mut line) {
                Ok(length) => length,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                    self.send_copy_fail(COPY_CANCELLED)?;
                    return Ok(CopyInputEnd::Cancelled);
                }
                Err(_) => return self.fail_session(),
            };
            if interrupt_requested() {
                self.send_copy_fail(COPY_CANCELLED)?;
                return Ok(CopyInputEnd::Cancelled);
            }
            if length == 0 {
                self.send_copy_fail(COPY_EOF)?;
                return Ok(CopyInputEnd::Incomplete);
            }
            if control_line(&line) == "\\." {
                self.send_message(b'c', &[])?;
                return Ok(CopyInputEnd::Completed);
            }
            self.send_message(b'd', line.as_bytes())?;
        }
    }

    fn send_copy_fail(&mut self, reason: &str) -> ShellResult<()> {
        let mut payload = Vec::with_capacity(reason.len() + 1);
        payload.extend_from_slice(reason.as_bytes());
        payload.push(0);
        self.send_message(b'f', &payload)
    }

    fn send_message(&mut self, kind: u8, payload: &[u8]) -> ShellResult<()> {
        let length = payload
            .len()
            .checked_add(4)
            .and_then(|length| u32::try_from(length).ok())
            .ok_or(BackendShellError::SessionFailed)?;
        self.stream
            .write_all(&[kind])
            .and_then(|()| self.stream.write_all(&length.to_be_bytes()))
            .and_then(|()| self.stream.write_all(payload))
            .map_err(|_| BackendShellError::SessionFailed)
    }

    fn read_message(&mut self) -> ShellResult<BackendMessage> {
        self.read_message_inner(true)
    }

    fn read_message_inner(&mut self, cancellable: bool) -> ShellResult<BackendMessage> {
        let mut header = [0_u8; 5];
        self.read_exact(&mut header, cancellable)?;
        let length = u32::from_be_bytes(
            header[1..]
                .try_into()
                .map_err(|_| BackendShellError::SessionFailed)?,
        ) as usize;
        if !(4..=MAXIMUM_MESSAGE_LENGTH).contains(&length) {
            return self.fail_session();
        }
        let mut payload = vec![0_u8; length - 4];
        self.read_exact(&mut payload, cancellable)?;
        Ok(BackendMessage {
            kind: header[0],
            payload,
        })
    }

    fn read_exact(&mut self, output: &mut [u8], cancellable: bool) -> ShellResult<()> {
        let mut offset = 0;
        while offset < output.len() {
            if interrupt_requested() {
                if cancellable && self.active_query {
                    self.cancel_query()?;
                } else {
                    return self.fail_session();
                }
            }
            match self.stream.read(&mut output[offset..]) {
                Ok(0) => return self.fail_session(),
                Ok(length) => offset += length,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return self.fail_session(),
            }
        }
        Ok(())
    }

    fn cancel_query(&mut self) -> ShellResult<()> {
        if self.cancel_sent {
            return Ok(());
        }
        let mut stream =
            UnixStream::connect(&self.socket_path).map_err(|_| BackendShellError::SessionFailed)?;
        let mut request = [0_u8; 16];
        request[..4].copy_from_slice(&16_u32.to_be_bytes());
        request[4..8].copy_from_slice(&CANCEL_REQUEST_CODE.to_be_bytes());
        request[8..12].copy_from_slice(&self.backend_pid.to_be_bytes());
        request[12..].copy_from_slice(&self.secret_key.to_be_bytes());
        stream
            .write_all(&request)
            .map_err(|_| BackendShellError::SessionFailed)?;
        self.cancel_sent = true;
        Ok(())
    }

    fn fail_session<T>(&mut self) -> ShellResult<T> {
        let _ = self.terminate();
        Err(BackendShellError::SessionFailed)
    }
}

impl Drop for BackendSession {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

struct BackendMessage {
    kind: u8,
    payload: Vec<u8>,
}

#[derive(Clone, Copy)]
enum CopyInputEnd {
    Completed,
    Cancelled,
    Incomplete,
}

fn fixed_socket_path(config: &Config) -> ShellResult<PathBuf> {
    let [Host::Unix(directory)] = config.get_hosts() else {
        return Err(BackendShellError::AttachFailed);
    };
    let [port] = config.get_ports() else {
        return Err(BackendShellError::AttachFailed);
    };
    Ok(directory.join(format!(".s.PGSQL.{port}")))
}

fn require_parameter_status(payload: &[u8]) -> ShellResult<()> {
    let mut body = Body::new(payload);
    body.string()?;
    body.string()?;
    body.finish()
}

fn require_ready(payload: &[u8]) -> ShellResult<u8> {
    let [status @ (b'I' | b'T' | b'E')] = payload else {
        return Err(BackendShellError::SessionFailed);
    };
    Ok(*status)
}

fn parse_row_description(payload: &[u8]) -> ShellResult<Vec<String>> {
    let mut body = Body::new(payload);
    let count = body.u16()? as usize;
    let mut names = Vec::with_capacity(count);
    for _ in 0..count {
        names.push(body.string()?.to_owned());
        body.bytes(4 + 2 + 4 + 2 + 4)?;
        if body.u16()? != 0 {
            return Err(BackendShellError::SessionFailed);
        }
    }
    body.finish()?;
    Ok(names)
}

fn parse_data_row(payload: &[u8], expected: usize) -> ShellResult<Vec<Option<String>>> {
    let mut body = Body::new(payload);
    if body.u16()? as usize != expected {
        return Err(BackendShellError::SessionFailed);
    }
    let mut values = Vec::with_capacity(expected);
    for _ in 0..expected {
        let length = body.i32()?;
        if length == -1 {
            values.push(None);
        } else if length >= 0 {
            let value = str::from_utf8(body.bytes(length as usize)?)
                .map_err(|_| BackendShellError::SessionFailed)?;
            values.push(Some(value.to_owned()));
        } else {
            return Err(BackendShellError::SessionFailed);
        }
    }
    body.finish()?;
    Ok(values)
}

fn require_text_copy(payload: &[u8]) -> ShellResult<()> {
    let mut body = Body::new(payload);
    if body.u8()? != 0 {
        return Err(BackendShellError::SessionFailed);
    }
    let columns = body.u16()? as usize;
    for _ in 0..columns {
        if body.u16()? != 0 {
            return Err(BackendShellError::SessionFailed);
        }
    }
    body.finish()
}

fn render_command_tag(payload: &[u8], output: &mut impl Write) -> ShellResult<()> {
    let mut body = Body::new(payload);
    let tag = body.string()?;
    body.finish()?;
    output
        .write_all(b"COMMAND ")
        .map_err(|_| BackendShellError::SessionFailed)?;
    write_escaped(tag, output)?;
    output
        .write_all(b"\n")
        .map_err(|_| BackendShellError::SessionFailed)
}

fn render_transaction_status(status: u8, output: &mut impl Write) -> ShellResult<()> {
    output
        .write_all(b"TRANSACTION ")
        .and_then(|()| output.write_all(&[status, b'\n']))
        .map_err(|_| BackendShellError::SessionFailed)
}

fn render_database_message(
    label: &str,
    payload: &[u8],
    output: &mut impl Write,
) -> ShellResult<()> {
    let message = DatabaseMessage::parse(payload)?;
    write!(output, "{label} {}: ", message.code).map_err(|_| BackendShellError::SessionFailed)?;
    write_escaped(message.message, output)?;
    output
        .write_all(b"\n")
        .map_err(|_| BackendShellError::SessionFailed)?;
    for (field_label, value) in [("DETAIL", message.detail), ("HINT", message.hint)] {
        if let Some(value) = value {
            write!(output, "{field_label}: ").map_err(|_| BackendShellError::SessionFailed)?;
            write_escaped(value, output)?;
            output
                .write_all(b"\n")
                .map_err(|_| BackendShellError::SessionFailed)?;
        }
    }
    Ok(())
}

fn render_fields<'a>(
    fields: impl IntoIterator<Item = Option<&'a str>>,
    output: &mut impl Write,
) -> ShellResult<()> {
    let mut separator = b"".as_slice();
    for field in fields {
        output
            .write_all(separator)
            .map_err(|_| BackendShellError::SessionFailed)?;
        match field {
            Some(value) => write_escaped(value, output)?,
            None => output
                .write_all(b"<NULL>")
                .map_err(|_| BackendShellError::SessionFailed)?,
        }
        separator = b"\t";
    }
    output
        .write_all(b"\n")
        .map_err(|_| BackendShellError::SessionFailed)
}

pub(super) fn write_escaped(value: &str, output: &mut impl Write) -> ShellResult<()> {
    for character in value.chars() {
        match character {
            '\\' => output.write_all(b"\\\\"),
            '\t' => output.write_all(b"\\t"),
            '\r' => output.write_all(b"\\r"),
            '\n' => output.write_all(b"\\n"),
            '\u{1b}' => output.write_all(b"\\e"),
            '\u{7f}' => output.write_all(b"\\x7f"),
            character if character.is_control() => write!(output, "\\u{{{:x}}}", character as u32),
            character => write!(output, "{character}"),
        }
        .map_err(|_| BackendShellError::SessionFailed)?;
    }
    Ok(())
}

fn write_escaped_bytes(value: &[u8], output: &mut impl Write) -> ShellResult<()> {
    write_escaped(
        str::from_utf8(value).map_err(|_| BackendShellError::SessionFailed)?,
        output,
    )
}

fn control_line(line: &str) -> &str {
    line.strip_suffix('\n')
        .and_then(|line| line.strip_suffix('\r').or(Some(line)))
        .unwrap_or(line)
}

struct DatabaseMessage<'a> {
    code: &'a str,
    message: &'a str,
    detail: Option<&'a str>,
    hint: Option<&'a str>,
}

impl<'a> DatabaseMessage<'a> {
    fn parse(payload: &'a [u8]) -> ShellResult<Self> {
        let mut body = Body::new(payload);
        let mut code = None;
        let mut message = None;
        let mut detail = None;
        let mut hint = None;
        loop {
            let field = body.u8()?;
            if field == 0 {
                break;
            }
            let value = body.string()?;
            match field {
                b'C' if code.replace(value).is_some() => {
                    return Err(BackendShellError::SessionFailed);
                }
                b'M' if message.replace(value).is_some() => {
                    return Err(BackendShellError::SessionFailed);
                }
                b'D' if detail.replace(value).is_some() => {
                    return Err(BackendShellError::SessionFailed);
                }
                b'H' if hint.replace(value).is_some() => {
                    return Err(BackendShellError::SessionFailed);
                }
                _ => {}
            }
        }
        body.finish()?;
        Ok(Self {
            code: code.ok_or(BackendShellError::SessionFailed)?,
            message: message.ok_or(BackendShellError::SessionFailed)?,
            detail,
            hint,
        })
    }
}

struct Body<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Body<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn u8(&mut self) -> ShellResult<u8> {
        Ok(self.bytes(1)?[0])
    }

    fn u16(&mut self) -> ShellResult<u16> {
        Ok(u16::from_be_bytes(
            self.bytes(2)?
                .try_into()
                .map_err(|_| BackendShellError::SessionFailed)?,
        ))
    }

    fn i32(&mut self) -> ShellResult<i32> {
        Ok(i32::from_be_bytes(
            self.bytes(4)?
                .try_into()
                .map_err(|_| BackendShellError::SessionFailed)?,
        ))
    }

    fn bytes(&mut self, length: usize) -> ShellResult<&'a [u8]> {
        let end = self
            .offset
            .checked_add(length)
            .filter(|end| *end <= self.bytes.len())
            .ok_or(BackendShellError::SessionFailed)?;
        let bytes = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(bytes)
    }

    fn string(&mut self) -> ShellResult<&'a str> {
        let relative_end = self.bytes[self.offset..]
            .iter()
            .position(|byte| *byte == 0)
            .ok_or(BackendShellError::SessionFailed)?;
        let bytes = self.bytes(relative_end)?;
        self.offset += 1;
        str::from_utf8(bytes).map_err(|_| BackendShellError::SessionFailed)
    }

    fn finish(self) -> ShellResult<()> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(BackendShellError::SessionFailed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io::Cursor, os::unix::net::UnixListener, path::Path, thread};

    fn session_pair() -> (BackendSession, UnixStream) {
        let (client, server) = UnixStream::pair().expect("Unix stream pair");
        (
            BackendSession {
                stream: client,
                socket_path: PathBuf::from("/unreachable-test-socket"),
                backend_pid: 2,
                secret_key: 3,
                active_query: false,
                cancel_sent: false,
                terminated: false,
            },
            server,
        )
    }

    fn read_frontend(stream: &mut UnixStream) -> (u8, Vec<u8>) {
        let mut header = [0_u8; 5];
        stream.read_exact(&mut header).expect("frontend header");
        let length = u32::from_be_bytes(header[1..].try_into().expect("message length")) as usize;
        assert!(length >= 4);
        let mut payload = vec![0_u8; length - 4];
        stream.read_exact(&mut payload).expect("frontend payload");
        (header[0], payload)
    }

    fn send_backend(stream: &mut UnixStream, kind: u8, payload: &[u8]) {
        stream.write_all(&[kind]).expect("backend kind");
        stream
            .write_all(&u32::try_from(payload.len() + 4).unwrap().to_be_bytes())
            .expect("backend length");
        stream.write_all(payload).expect("backend payload");
    }

    fn text_row_description(name: &str) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&1_u16.to_be_bytes());
        payload.extend_from_slice(name.as_bytes());
        payload.push(0);
        payload.extend_from_slice(&[0; 16]);
        payload.extend_from_slice(&0_u16.to_be_bytes());
        payload
    }

    #[test]
    fn rejects_binary_result_and_copy_formats() {
        let mut row_description = Vec::new();
        row_description.extend_from_slice(&1_u16.to_be_bytes());
        row_description.extend_from_slice(b"value\0");
        row_description.extend_from_slice(&[0; 16]);
        row_description.extend_from_slice(&1_u16.to_be_bytes());
        assert!(parse_row_description(&row_description).is_err());
        assert!(require_text_copy(&[1, 0, 0]).is_err());
        assert!(require_text_copy(&[0, 0, 1, 0, 1]).is_err());
    }

    #[test]
    fn renders_complete_database_message_fields_safely() {
        let payload = b"SERROR\0C23505\0Mbad\trow\0Ddetail\0Hhint\0\0";
        let mut output = Vec::new();
        render_database_message("ERROR", payload, &mut output).expect("render error");
        assert_eq!(
            output,
            b"ERROR 23505: bad\\trow\nDETAIL: detail\nHINT: hint\n"
        );
    }

    #[test]
    fn data_rows_preserve_null_and_text() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&2_u16.to_be_bytes());
        payload.extend_from_slice(&(-1_i32).to_be_bytes());
        payload.extend_from_slice(&4_i32.to_be_bytes());
        payload.extend_from_slice(b"NULL");
        assert_eq!(
            parse_data_row(&payload, 2).expect("data row"),
            vec![None, Some(String::from("NULL"))]
        );
    }

    #[test]
    fn drives_simple_query_frames_through_ready_for_query() {
        let (mut session, mut server) = session_pair();
        let server_thread = thread::spawn(move || {
            assert_eq!(read_frontend(&mut server), (b'Q', b"SELECT 1\n\0".to_vec()));
            send_backend(&mut server, b'T', &text_row_description("name"));
            let mut row = Vec::new();
            row.extend_from_slice(&1_u16.to_be_bytes());
            row.extend_from_slice(&3_i32.to_be_bytes());
            row.extend_from_slice(b"a\tb");
            send_backend(&mut server, b'D', &row);
            send_backend(&mut server, b'N', b"SNOTICE\0C00000\0Mhello\0\0");
            send_backend(&mut server, b'C', b"SELECT 1\0");
            send_backend(&mut server, b'Z', b"I");
            assert_eq!(read_frontend(&mut server), (b'X', Vec::new()));
        });
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        session
            .execute("SELECT 1\n", &mut input, &mut output)
            .expect("simple query");
        session.terminate().expect("terminate");
        server_thread.join().expect("server thread");
        assert_eq!(
            output,
            b"name\na\\tb\nNOTICE 00000: hello\nCOMMAND SELECT 1\nTRANSACTION I\n"
        );
    }

    #[test]
    fn drives_text_copy_in_and_copy_out() {
        let (mut session, mut server) = session_pair();
        let server_thread = thread::spawn(move || {
            assert_eq!(read_frontend(&mut server).0, b'Q');
            send_backend(&mut server, b'G', &[0, 0, 1, 0, 0]);
            assert_eq!(read_frontend(&mut server), (b'd', b"one\n".to_vec()));
            assert_eq!(read_frontend(&mut server), (b'c', Vec::new()));
            send_backend(&mut server, b'C', b"COPY 1\0");
            send_backend(&mut server, b'Z', b"I");

            assert_eq!(read_frontend(&mut server).0, b'Q');
            send_backend(&mut server, b'H', &[0, 0, 1, 0, 0]);
            send_backend(&mut server, b'd', b"a\tb\n");
            send_backend(&mut server, b'c', &[]);
            send_backend(&mut server, b'C', b"COPY 1\0");
            send_backend(&mut server, b'Z', b"I");
            assert_eq!(read_frontend(&mut server), (b'X', Vec::new()));
        });
        let mut input = Cursor::new(b"one\n\\.\n".to_vec());
        let mut output = Vec::new();
        session
            .execute("COPY input", &mut input, &mut output)
            .expect("copy input");
        session
            .execute("COPY output", &mut input, &mut output)
            .expect("copy output");
        session.terminate().expect("terminate");
        server_thread.join().expect("server thread");
        assert_eq!(
            output,
            b"copy=> copy=> COMMAND COPY 1\nTRANSACTION I\na\\tb\\n\nCOMMAND COPY 1\nTRANSACTION I\n"
        );
        assert_eq!(COPY_EOF, "Orna COPY input ended before \\.");
    }

    #[test]
    fn sends_the_exact_cancel_request_to_the_fixed_socket() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("server crate remains below crates");
        let socket_path = repository
            .join("target")
            .join(format!("backend-shell-cancel-{}.sock", std::process::id()));
        let _ = fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path).expect("cancel listener");
        let listener_thread = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("cancel connection");
            let mut request = [0_u8; 16];
            stream.read_exact(&mut request).expect("cancel request");
            request
        });
        let (mut session, _server) = session_pair();
        session.socket_path = socket_path.clone();
        session.cancel_query().expect("cancel query");
        let request = listener_thread.join().expect("listener thread");
        let _ = fs::remove_file(socket_path);
        assert_eq!(&request[..4], &16_u32.to_be_bytes());
        assert_eq!(&request[4..8], &CANCEL_REQUEST_CODE.to_be_bytes());
        assert_eq!(&request[8..12], &2_u32.to_be_bytes());
        assert_eq!(&request[12..], &3_u32.to_be_bytes());
    }
}
