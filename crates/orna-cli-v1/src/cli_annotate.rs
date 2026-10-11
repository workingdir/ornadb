//! `orna annotate FILE --type MEDIA_TYPE --output FILE [--suffix HINT]`:
//! encode raw file bytes and their canonical MIME-1 annotation as an OVB-2 Blob.
//! This command is repository-independent and never reads or writes a repository.

use std::{fs::File, path::Path};

use orna_value_v1::{MimeRegistry, encode_ovb2_reader_to_writer};

use super::Diagnostic;

const USAGE: &str = "usage: orna annotate FILE --type MEDIA_TYPE --output FILE [--suffix HINT]";

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
    let mut source = File::open(source_path).map_err(|error| {
        Diagnostic::target_with_detail(
            "E2001",
            "annotate source could not be opened",
            "check that the source file exists and is readable",
            format!("{}: {error}", source_path.display()),
        )
    })?;
    let metadata = source.metadata().map_err(|error| {
        Diagnostic::target_with_detail(
            "E2001",
            "annotate source metadata could not be read",
            "check that the source file is readable",
            format!("{}: {error}", source_path.display()),
        )
    })?;
    if !metadata.is_file() {
        return Err(Diagnostic::target(
            "E2001",
            "annotate source is not a regular file",
            "supply a readable regular file",
        ));
    }
    let source_length = metadata.len();

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
        u64::MAX,
    );
    if let Err(error) = encoded {
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
        let (title, help) = match error {
            orna_value_v1::Error::InvalidMediaType
            | orna_value_v1::Error::InvalidSuffix
            | orna_value_v1::Error::IncompatibleSuffix
            | orna_value_v1::Error::NonCanonical => (
                "annotate media annotation is invalid",
                "choose a valid MIME-1 type and compatible suffix",
            ),
            _ => (
                "annotated Blob could not be encoded",
                "check the source file and destination storage",
            ),
        };
        return Err(Diagnostic::target_with_detail(
            "E2001",
            title,
            help,
            format!("{error:?}"),
        ));
    }
    if !source
        .metadata()
        .is_ok_and(|metadata| metadata.len() == source_length)
    {
        let cleanup = cleanup_staging_file(output).err();
        return Err(Diagnostic::target_with_detail(
            "E2001",
            "annotate source length could not be confirmed unchanged",
            "retry with a stable source file",
            cleanup.unwrap_or_else(|| "temporary staging file removed".to_owned()),
        ));
    }
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

    fn words(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
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
