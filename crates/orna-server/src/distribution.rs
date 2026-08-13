//! Installed Orna distribution evidence verification.

use std::{
    fs,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};

use orna_postgres::ENGINE_MANIFEST;
use sha2::{Digest, Sha256};

const DISTRIBUTION_MANIFEST_PATH: &str = "/usr/share/orna/distribution-manifest.toml";
const MAXIMUM_MANIFEST_BYTES: u64 = 16 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct DistributionError;

pub(super) fn verify_current_distribution() -> Result<(), DistributionError> {
    let bytes = read_manifest(Path::new(DISTRIBUTION_MANIFEST_PATH))?;
    let engine_sha256 = digest_bytes(ENGINE_MANIFEST);
    let executable_sha256 = digest_file(Path::new("/proc/self/exe"))?;
    validate_manifest(&bytes, &engine_sha256, &executable_sha256)
}

fn read_manifest(path: &Path) -> Result<Vec<u8>, DistributionError> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| DistributionError)?;
    let metadata = file.metadata().map_err(|_| DistributionError)?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.gid() != 0
        || metadata.mode() & 0o7777 != 0o644
        || metadata.nlink() != 1
        || metadata.len() > MAXIMUM_MANIFEST_BYTES
    {
        return Err(DistributionError);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut bytes)
        .map_err(|_| DistributionError)?;
    if bytes.len() as u64 != metadata.len() {
        return Err(DistributionError);
    }
    Ok(bytes)
}

fn digest_file(path: &Path) -> Result<String, DistributionError> {
    let mut file = fs::File::open(path).map_err(|_| DistributionError)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|_| DistributionError)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex(digest.finalize()))
}

fn digest_bytes(bytes: &[u8]) -> String {
    hex(Sha256::digest(bytes))
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(DIGITS[(byte >> 4) as usize] as char);
        encoded.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn validate_manifest(
    bytes: &[u8],
    engine_sha256: &str,
    executable_sha256: &str,
) -> Result<(), DistributionError> {
    let text = std::str::from_utf8(bytes).map_err(|_| DistributionError)?;
    let text = text.strip_suffix('\n').ok_or(DistributionError)?;
    let lines = text.split('\n').collect::<Vec<_>>();
    if lines.len() != 11
        || lines[0] != "format = 1"
        || !quoted_with_prefix(lines[1], "rustc = ", "rustc ")
        || !quoted_with_prefix(lines[2], "cargo = ", "cargo ")
        || !quoted_digest(lines[3], "builder_image = ", "sha256:")
        || !quoted_digest(lines[4], "cargo_lock_sha256 = ", "")
        || lines[5] != "rust_path_remap = \"<repository>=/usr/src/orna\""
        || lines[6] != "rust_link_flags = \"-C link-arg=-Wl,--build-id=none\""
        || !quoted_equals(
            lines[7],
            "embedded_engine_manifest_sha256 = ",
            engine_sha256,
        )
        || lines[8] != "accepted_predecessor_engines = []"
        || lines[9] != "supported_forward_edges = []"
        || !quoted_equals(lines[10], "executable_sha256 = ", executable_sha256)
    {
        return Err(DistributionError);
    }
    Ok(())
}

fn quoted_with_prefix(line: &str, key: &str, value_prefix: &str) -> bool {
    quoted_value(line, key).is_some_and(|value| {
        value.starts_with(value_prefix)
            && value.len() > value_prefix.len()
            && !value.chars().any(char::is_control)
    })
}

fn quoted_digest(line: &str, key: &str, digest_prefix: &str) -> bool {
    quoted_value(line, key)
        .is_some_and(|value| value.strip_prefix(digest_prefix).is_some_and(is_sha256))
}

fn quoted_equals(line: &str, key: &str, expected: &str) -> bool {
    quoted_value(line, key) == Some(expected)
}

fn quoted_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.strip_prefix(key)?.strip_prefix('"')?.strip_suffix('"')
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENGINE: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const EXECUTABLE: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    fn manifest() -> Vec<u8> {
        format!(
            "format = 1\n\
rustc = \"rustc 1.95.0 (build)\"\n\
cargo = \"cargo 1.95.0 (build)\"\n\
builder_image = \"sha256:3333333333333333333333333333333333333333333333333333333333333333\"\n\
cargo_lock_sha256 = \"4444444444444444444444444444444444444444444444444444444444444444\"\n\
rust_path_remap = \"<repository>=/usr/src/orna\"\n\
rust_link_flags = \"-C link-arg=-Wl,--build-id=none\"\n\
embedded_engine_manifest_sha256 = \"{ENGINE}\"\n\
accepted_predecessor_engines = []\n\
supported_forward_edges = []\n\
executable_sha256 = \"{EXECUTABLE}\"\n"
        )
        .into_bytes()
    }

    #[test]
    fn accepts_only_the_bound_current_distribution() {
        validate_manifest(&manifest(), ENGINE, EXECUTABLE).expect("valid distribution");

        for changed in [
            ("format = 1", "format = 2"),
            (
                "accepted_predecessor_engines = []",
                "accepted_predecessor_engines = [\"x\"]",
            ),
            (
                "supported_forward_edges = []",
                "supported_forward_edges = [\"x\"]",
            ),
            (
                ENGINE,
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ),
            (
                EXECUTABLE,
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            ),
        ] {
            let changed = String::from_utf8(manifest())
                .expect("manifest text")
                .replace(changed.0, changed.1);
            assert_eq!(
                validate_manifest(changed.as_bytes(), ENGINE, EXECUTABLE),
                Err(DistributionError)
            );
        }
    }

    #[test]
    fn rejects_malformed_or_unbounded_evidence_fields() {
        for bytes in [
            manifest()[..manifest().len() - 1].to_vec(),
            [manifest(), b"extra = true\n".to_vec()].concat(),
            String::from_utf8(manifest())
                .expect("manifest text")
                .replace("rustc 1.95.0 (build)", "rustc \nmalicious")
                .into_bytes(),
            String::from_utf8(manifest())
                .expect("manifest text")
                .replace("sha256:3333", "sha256:zzzz")
                .into_bytes(),
        ] {
            assert_eq!(
                validate_manifest(&bytes, ENGINE, EXECUTABLE),
                Err(DistributionError)
            );
        }
    }

    #[test]
    fn maps_file_failures_to_one_opaque_error() {
        let missing = Path::new("/definitely/not/an/orna/distribution-manifest");
        assert_eq!(read_manifest(missing), Err(DistributionError));
        assert_eq!(digest_file(missing), Err(DistributionError));
    }

    #[test]
    fn digest_is_lowercase_sha256() {
        assert_eq!(
            digest_bytes(b"orna"),
            "3ea53f51c6c9f57e94378af053fd1668a5f88a08d946a3e21a512ffa45578ecb"
        );
    }
}
