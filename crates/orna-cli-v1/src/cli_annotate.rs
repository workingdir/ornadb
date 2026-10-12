//! `orna annotate FILE --type MEDIA_TYPE --output FILE [--suffix HINT]`:
//! encode raw file bytes and their canonical MIME-1 annotation as an OVB-2 Blob.
//! This command is repository-independent and never reads or writes a repository.

use std::path::Path;

use orna_sys_v1::{CaptureFileError, CaptureFileStream, FilesystemProvider};
use orna_value_v1::{MimeRegistry, encode_ovb2_reader_to_writer};

use super::Diagnostic;

const USAGE: &str = "usage: orna annotate FILE --type MEDIA_TYPE --output FILE [--suffix HINT]";

const MAX_SOURCE_BYTES: u64 = i64::MAX as u64;

#[derive(Debug, Eq, PartialEq)]
struct AnnotateOptions<'a> {
    source: &'a str,
    output: &'a str,
    media_type: &'a str,
    suffix: Option<&'a str>,
}

fn parse_options(arguments: &[String]) -> Result<AnnotateOptions<'_>, Diagnostic> {
    let mut source = None;
    let mut output = None;
    let mut media_type = None;
    let mut suffix = None;
    let mut words = arguments.iter().map(String::as_str);
    while let Some(word) = words.next() {
        match word {
            "--output" => {
                if output.is_some() {
                    return Err(usage("`--output` was specified more than once", USAGE));
                }
                output = Some(
                    words
                        .next()
                        .ok_or_else(|| usage("`--output` needs a value", USAGE))?,
                );
            }
            "--type" => {
                if media_type.is_some() {
                    return Err(usage("`--type` was specified more than once", USAGE));
                }
                media_type = Some(
                    words
                        .next()
                        .ok_or_else(|| usage("`--type` needs a value", USAGE))?,
                );
            }
            "--suffix" => {
                if suffix.is_some() {
                    return Err(usage("`--suffix` was specified more than once", USAGE));
                }
                suffix = Some(
                    words
                        .next()
                        .ok_or_else(|| usage("`--suffix` needs a value", USAGE))?,
                );
            }
            option if option.starts_with('-') => {
                return Err(usage("unsupported annotate option", USAGE));
            }
            path if source.is_none() => source = Some(path),
            _ => return Err(usage("annotate accepts one source file", USAGE)),
        }
    }
    Ok(AnnotateOptions {
        source: source.ok_or_else(|| usage("annotate needs a source file", USAGE))?,
        output: output.ok_or_else(|| usage("annotate needs an output file", USAGE))?,
        media_type: media_type.ok_or_else(|| usage("annotate needs a media type", USAGE))?,
        suffix,
    })
}

pub(super) fn run(arguments: &[String]) -> Result<(), Diagnostic> {
    let options = parse_options(arguments)?;
    let annotation = MimeRegistry::mime1()
        .canonical_annotation(options.media_type, options.suffix)
        .map_err(|error| {
            Diagnostic::target_with_detail(
                "E2001",
                "annotate media annotation is invalid",
                "choose a valid MIME-1 type and compatible suffix",
                format!("{error:?}"),
            )
        })?;

    let source_path = Path::new(options.source);
    let mut source = open_source(source_path)?;
    let source_length = source.length();

    let output_path = Path::new(options.output);
    let output_parent = output_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut output = tempfile::Builder::new()
        .prefix(".orna-annotate-")
        .tempfile_in(output_parent)
        .map_err(|error| {
            Diagnostic::target_with_detail(
                "E2001",
                "annotate output could not be staged",
                "check that the destination directory is writable",
                format!("{}: {error}", output_parent.display()),
            )
        })?;
    let encoded = encode_ovb2_reader_to_writer(
        &mut source,
        source_length,
        &annotation,
        &mut output,
        MAX_SOURCE_BYTES,
    );
    if let Err(error) = encoded {
        if let Err(source_change) = source.verify_unchanged() {
            let detail = cleanup_staging_file(output).err().unwrap_or_default();
            return Err(source_error(source_path, source_change, detail));
        }
        if let Err(cleanup) = cleanup_staging_file(output) {
            return Err(Diagnostic::target_with_detail(
                "E2001",
                "failed annotate output could not be removed",
                "remove the temporary staging file listed in the detail",
                format!("{error:?}; {cleanup}"),
            ));
        }
        if error == orna_value_v1::Error::Limit {
            return Err(Diagnostic::usage(
                "E1002",
                "annotate source exceeds the OVB-2 format maximum",
                "choose a source smaller than 2^63 bytes",
            ));
        }
        return Err(Diagnostic::target_with_detail(
            "E2001",
            "annotated Blob could not be encoded",
            "check the source file and destination storage",
            format!("{error:?}"),
        ));
    }
    let output = verify_source_before_install(source_path, &source, output)?;

    if let Err(error) = output.as_file().sync_all() {
        let cleanup = cleanup_staging_file(output).err();
        return Err(Diagnostic::target_with_detail(
            "E2001",
            "annotated Blob could not be written",
            "check the destination directory and available storage",
            cleanup.map_or_else(
                || error.to_string(),
                |cleanup| format!("{error}; {cleanup}"),
            ),
        ));
    }
    if let Err(error) = output.persist_noclobber(output_path) {
        let persist_error = error.error;
        let cleanup = cleanup_staging_file(error.file).err();
        return Err(Diagnostic::target_with_detail(
            "E2001",
            "annotated Blob could not be installed",
            "choose a new writable destination file",
            cleanup.map_or_else(
                || format!("{}: {persist_error}", output_path.display()),
                |cleanup| format!("{}: {persist_error}; {cleanup}", output_path.display()),
            ),
        ));
    }
    Ok(())
}

fn open_source(path: &Path) -> Result<CaptureFileStream, Diagnostic> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        source_error(
            path,
            CaptureFileError::SourceUnavailable,
            format!("{error}"),
        )
    })?;
    if !metadata.file_type().is_file() {
        return Err(source_error(
            path,
            CaptureFileError::InvalidPath,
            "source is not a regular file".to_owned(),
        ));
    }

    let root = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let relative_path = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            source_error(
                path,
                CaptureFileError::InvalidPath,
                "source path has no UTF-8 file name".to_owned(),
            )
        })?;
    let root_text = root.to_str().ok_or_else(|| {
        source_error(
            path,
            CaptureFileError::InvalidPath,
            "source parent path is not UTF-8".to_owned(),
        )
    })?;
    let max_bytes = MAX_SOURCE_BYTES.min(usize::MAX as u64);
    let mut filesystem =
        FilesystemProvider::with_limits(max_bytes as usize, 1).map_err(|error| {
            Diagnostic::target_with_detail(
                "E2001",
                "annotate source could not be opened",
                "check that the source file exists and is readable",
                format!("{}: {error:?}", path.display()),
            )
        })?;
    filesystem.allow_root(root).map_err(|error| {
        Diagnostic::target_with_detail(
            "E2001",
            "annotate source could not be opened",
            "check that the source file exists and is readable",
            format!("{}: {error:?}", path.display()),
        )
    })?;
    filesystem
        .capture_file_stream(root_text, relative_path, max_bytes)
        .map_err(|error| source_error(path, error, String::new()))
}

fn verify_source_before_install(
    path: &Path,
    source: &CaptureFileStream,
    output: tempfile::NamedTempFile,
) -> Result<tempfile::NamedTempFile, Diagnostic> {
    if let Err(error) = source.verify_unchanged() {
        let detail = cleanup_staging_file(output).err().unwrap_or_default();
        return Err(source_error(path, error, detail));
    }
    Ok(output)
}

fn source_error(path: &Path, error: CaptureFileError, detail: String) -> Diagnostic {
    let code_detail = format!(
        "{}: {} ({})",
        path.display(),
        error.failure_code(),
        error.code()
    );
    let detail = if detail.is_empty() {
        code_detail
    } else {
        format!("{code_detail}: {detail}")
    };
    match error {
        CaptureFileError::LimitExceeded => Diagnostic::usage(
            "E1002",
            "annotate source exceeds the OVB-2 format maximum",
            "choose a source smaller than 2^63 bytes",
        ),
        CaptureFileError::InvalidPath => Diagnostic::target_with_detail(
            "E2001",
            "annotate source is not a regular file",
            "supply a regular file without symbolic-link path components",
            detail,
        ),
        CaptureFileError::SourceChanged => Diagnostic::target_with_detail(
            "E2001",
            "annotate source changed while being read",
            "retry with a stable source file",
            detail,
        ),
        CaptureFileError::UnauthorizedRoot | CaptureFileError::SourceUnavailable => {
            Diagnostic::target_with_detail(
                "E2001",
                "annotate source could not be opened",
                "check that the source file exists and is readable",
                detail,
            )
        }
    }
}

fn cleanup_staging_file(output: tempfile::NamedTempFile) -> Result<(), String> {
    let path = output.path().display().to_string();
    output.close().map_err(|error| format!("{path}: {error}"))
}

fn usage(title: &'static str, help: &'static str) -> Diagnostic {
    Diagnostic::usage("E1002", title, help)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    fn words(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }
    #[test]
    fn rejects_same_length_source_mutation_before_installing_output() {
        use std::time::{Duration, SystemTime};

        let directory = tempfile::TempDir::new().unwrap();
        let fixture = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/media/tone.wav"
        ))
        .unwrap();
        let path = directory.path().join("tone.wav");
        std::fs::write(&path, &fixture).unwrap();

        let mut source = open_source(&path).unwrap();
        assert_eq!(
            io::copy(&mut source, &mut io::sink()).unwrap(),
            fixture.len() as u64
        );
        let output_path = directory.path().join("annotated.ovb2");
        std::fs::write(&output_path, b"existing output").unwrap();
        let mut output = tempfile::Builder::new()
            .prefix(".orna-annotate-")
            .tempfile_in(directory.path())
            .unwrap();
        std::io::Write::write_all(&mut output, b"partial OVB-2").unwrap();

        let mut changed = fixture.clone();
        let last_byte = changed.len() - 1;
        changed[last_byte] ^= 1;
        assert_eq!(changed.len(), fixture.len());
        std::fs::write(&path, &changed).unwrap();
        std::fs::File::open(&path)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new().set_modified(SystemTime::now() + Duration::from_secs(60)),
            )
            .unwrap();

        let error = verify_source_before_install(&path, &source, output).unwrap_err();
        assert!(format!("{error:?}").contains("ORNA-INGEST-005"));
        assert_eq!(std::fs::read(&output_path).unwrap(), b"existing output");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
    }

    #[test]
    fn parses_one_input_and_required_annotation_and_output() {
        assert_eq!(
            parse_options(&words(&[
                "song.wav",
                "--type",
                "audio/wav",
                "--output",
                "song.ovb2",
            ])),
            Ok(AnnotateOptions {
                source: "song.wav",
                output: "song.ovb2",
                media_type: "audio/wav",
                suffix: None,
            })
        );
        assert!(parse_options(&words(&["song.wav", "--type", "audio/wav"])).is_err());
        assert!(
            parse_options(&words(&[
                "song.wav",
                "--type",
                "audio/wav",
                "--type",
                "image/png",
                "--output",
                "out"
            ]))
            .is_err()
        );
    }
}
