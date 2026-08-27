use std::{io, io::Write, process};

fn main() {
    if let Err(error) = run() {
        eprintln!("runtime_demo: {error}");
        process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let document_body = "runtime_demo: terminal document\n";
    let document_body = document_body.as_bytes();
    let mut document_frame = Vec::with_capacity(
        b"ORNA-TERMINAL-DOCUMENT/1 ".len() + std::mem::size_of::<u32>() + document_body.len(),
    );
    document_frame.extend_from_slice(b"ORNA-TERMINAL-DOCUMENT/1 ");
    document_frame.extend_from_slice(&(document_body.len() as u32).to_be_bytes());
    document_frame.extend_from_slice(document_body);

    let media_type = b"application/octet-stream";
    let byte_stream_body = [0x00, 0xff, 0x01, 0xfe];
    let mut byte_stream_frame = Vec::with_capacity(
        b"ORNA-BYTE-STREAM/1 ".len()
            + std::mem::size_of::<u32>()
            + media_type.len()
            + std::mem::size_of::<u32>()
            + byte_stream_body.len(),
    );
    byte_stream_frame.extend_from_slice(b"ORNA-BYTE-STREAM/1 ");
    byte_stream_frame.extend_from_slice(&(media_type.len() as u32).to_be_bytes());
    byte_stream_frame.extend_from_slice(media_type);
    byte_stream_frame.extend_from_slice(&(byte_stream_body.len() as u32).to_be_bytes());
    byte_stream_frame.extend_from_slice(&byte_stream_body);

    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    orna_runtime_tty::render_document(&document_frame, &mut stdout)?;
    orna_runtime_tty::render_byte_stream(&byte_stream_frame, &mut stdout)?;
    stdout.flush()?;
    Ok(())
}
