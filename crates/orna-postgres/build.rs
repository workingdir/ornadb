use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

fn require_file(path: &Path) {
    assert!(
        path.is_file(),
        "missing embedded PostgreSQL build output: {}",
        path.display()
    );
}

fn main() {
    let crate_root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let repository = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("PostgreSQL crate must remain below crates/");
    println!(
        "cargo:rerun-if-changed={}",
        repository.join("postgresql").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        repository.join("third_party/postgresql").display()
    );
    println!("cargo:rerun-if-env-changed=ORNA_POSTGRES_ENGINE_OUTPUT");

    if env::var_os("CARGO_FEATURE_EMBEDDED").is_none() {
        return;
    }
    assert_eq!(env::var("CARGO_CFG_TARGET_OS").as_deref(), Ok("linux"));
    assert_eq!(env::var("CARGO_CFG_TARGET_ARCH").as_deref(), Ok("x86_64"));

    let output = if let Some(prebuilt) = env::var_os("ORNA_POSTGRES_ENGINE_OUTPUT") {
        let prebuilt = PathBuf::from(prebuilt);
        assert!(
            prebuilt.is_absolute(),
            "prebuilt engine output is not absolute"
        );
        prebuilt
    } else {
        let output_directory = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
        let target_root = output_directory.join("postgresql");
        let status = Command::new("make")
            .arg("-C")
            .arg(repository.join("postgresql"))
            .arg("manifest")
            .arg(format!("TARGET_ROOT={}", target_root.display()))
            .status()
            .expect("could not start the embedded PostgreSQL build");
        assert!(status.success(), "embedded PostgreSQL build failed");
        target_root.join("output")
    };
    for name in [
        "liborna_postgres18_initdb.a",
        "liborna_postgres18_backend.a",
        "embedded-postgresql-support.tar",
        "embedded-postgresql-support-manifest.json",
        "embedded-engine-manifest.json",
        "POSTGRESQL-LICENSE",
    ] {
        require_file(&output.join(name));
    }
    println!("cargo:rustc-link-search=native={}", output.display());
    println!("cargo:rustc-link-lib=static=orna_postgres18_initdb");
    println!("cargo:rustc-link-lib=static=orna_postgres18_backend");
    println!("cargo:rustc-link-lib=dylib=m");
    println!(
        "cargo:rustc-env=ORNA_POSTGRES_SUPPORT_BUNDLE={}",
        output.join("embedded-postgresql-support.tar").display()
    );
    println!(
        "cargo:rustc-env=ORNA_POSTGRES_SUPPORT_MANIFEST={}",
        output
            .join("embedded-postgresql-support-manifest.json")
            .display()
    );
    println!(
        "cargo:rustc-env=ORNA_POSTGRES_ENGINE_MANIFEST={}",
        output.join("embedded-engine-manifest.json").display()
    );
    println!(
        "cargo:rustc-env=ORNA_POSTGRES_LICENSE={}",
        output.join("POSTGRESQL-LICENSE").display()
    );
}
