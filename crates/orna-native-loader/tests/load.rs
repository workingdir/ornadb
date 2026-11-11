//! Loads real shared libraries that the test compiles with rustc, and checks the
//! digest, ABI and symbol outcomes of docs/design/native-libraries.md section 11.

use std::path::{Path, PathBuf};
use std::process::Command;

use orna_native_loader::{LoadError, NativeLibrary, load_verified};
use sha2::{Digest, Sha256};

/// A minimal native library. The ABI number and export names are edited in the
/// mismatch tests. It is inlined rather than stored under a `fixtures/` directory,
/// because crate-local fixture trees are pinned by the shared workspace manifest.
const LIBRARY_SOURCE: &str = r#"
const ABI_NUMBER: u32 = 1;

#[unsafe(no_mangle)]
pub extern "C" fn orna_library_abi() -> u32 {
    ABI_NUMBER
}

#[unsafe(no_mangle)]
pub extern "C" fn orna_text__slugify(_args: *const u8, _args_len: usize, _ctx: *const u8, _out: *mut u8) -> i32 {
    0
}
"#;

const SYMBOL: &str = "orna_text__slugify";

/// A compiled fixture library, with its directory kept alive for the test.
struct Built {
    _dir: tempfile::TempDir,
    path: PathBuf,
    sha256: [u8; 32],
}

fn build(source: &str) -> Built {
    let dir = tempfile::tempdir().expect("temporary directory");
    let source_path = dir.path().join("fixture.rs");
    std::fs::write(&source_path, source).expect("write fixture source");
    let path = dir
        .path()
        .join(format!("libfixture{}", std::env::consts::DLL_SUFFIX));
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let status = Command::new(rustc)
        .args(["--edition", "2024", "--crate-type", "cdylib", "-o"])
        .arg(&path)
        .arg(&source_path)
        .status()
        .expect("run rustc");
    assert!(
        status.success(),
        "rustc failed to build the fixture library"
    );
    let sha256 = Sha256::digest(std::fs::read(&path).expect("read fixture library")).into();
    Built {
        _dir: dir,
        path,
        sha256,
    }
}

fn load(path: &Path, sha256: &[u8; 32], abi: u32) -> Result<NativeLibrary, LoadError> {
    load_verified(path, sha256, abi)
}

#[test]
fn verified_library_loads_and_resolves_its_symbol() {
    let built = build(LIBRARY_SOURCE);
    let library = load(&built.path, &built.sha256, 1).expect("verified library loads");
    let symbol = library.symbol(SYMBOL).expect("exported symbol resolves");
    assert!(!symbol.address().is_null());
}

#[test]
fn digest_mismatch_is_refused_before_loading() {
    let built = build(LIBRARY_SOURCE);
    let wrong = [0_u8; 32];
    let error = load(&built.path, &wrong, 1)
        .err()
        .expect("digest mismatch is refused");
    assert_eq!(error.code(), "ORNA-STD-NATIVE-DIGEST");
    assert!(matches!(error, LoadError::Digest { .. }));
}

#[test]
fn abi_mismatch_is_refused() {
    let built =
        build(&LIBRARY_SOURCE.replace("const ABI_NUMBER: u32 = 1;", "const ABI_NUMBER: u32 = 2;"));
    let error = load(&built.path, &built.sha256, 1)
        .err()
        .expect("ABI mismatch is refused");
    assert_eq!(
        error,
        LoadError::Abi {
            expected: 1,
            found: Some(2),
        }
    );
    assert_eq!(error.code(), "ORNA-STD-NATIVE-ABI");
}

#[test]
fn missing_abi_export_is_refused() {
    let built = build(&LIBRARY_SOURCE.replace("orna_library_abi", "orna_renamed_abi"));
    let error = load(&built.path, &built.sha256, 1)
        .err()
        .expect("missing ABI export is refused");
    assert_eq!(
        error,
        LoadError::Abi {
            expected: 1,
            found: None,
        }
    );
}

#[test]
fn missing_symbol_is_refused() {
    let built = build(LIBRARY_SOURCE);
    let library = load(&built.path, &built.sha256, 1).expect("verified library loads");
    let error = library
        .symbol("orna_text__missing")
        .err()
        .expect("missing symbol is refused");
    assert_eq!(error.code(), "ORNA-STD-NATIVE-SYMBOL");
    assert_eq!(
        error,
        LoadError::Symbol {
            name: "orna_text__missing".to_owned(),
        }
    );
}

#[test]
fn unreadable_file_is_a_library_error() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let missing = dir.path().join("absent.so");
    let error = load(&missing, &[0_u8; 32], 1)
        .err()
        .expect("missing file is refused");
    assert_eq!(error.code(), "ORNA-STD-NATIVE-LIBRARY");
}
