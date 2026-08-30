use std::{
    env, fs,
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

fn prefix_backend_symbols(input: &Path, output: &Path, work_directory: &Path) {
    let _ = fs::remove_dir_all(work_directory);
    fs::create_dir_all(work_directory).expect("could not create Postgres archive work directory");
    let status = Command::new("ar")
        .arg("x")
        .arg(input)
        .current_dir(work_directory)
        .status()
        .expect("could not start ar while isolating embedded Postgres symbols");
    assert!(
        status.success(),
        "ar could not extract the embedded Postgres backend"
    );

    let objects = fs::read_dir(work_directory)
        .expect("could not list extracted embedded Postgres objects")
        .map(|entry| {
            entry
                .expect("could not read extracted Postgres object")
                .path()
        })
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    for object in &objects {
        for (original, replacement) in [
            ("make_date", "orna_postgres_make_date"),
            ("make_timestamp", "orna_postgres_make_timestamp"),
            ("to_timestamp", "orna_postgres_to_timestamp"),
        ] {
            let status = Command::new("objcopy")
                .arg("--redefine-sym")
                .arg(format!("{original}={replacement}"))
                .arg(object)
                .status()
                .expect("could not start objcopy while isolating embedded Postgres symbols");
            assert!(
                status.success(),
                "objcopy could not isolate the embedded Postgres backend symbol {original}"
            );
        }
    }

    let _ = fs::remove_file(output);
    let mut archive = Command::new("ar");
    archive.arg("crs").arg(output);
    for object in objects {
        archive.arg(object);
    }
    let status = archive
        .status()
        .expect("could not start ar while rebuilding the isolated Postgres archive");
    assert!(
        status.success(),
        "ar could not rebuild the isolated embedded Postgres backend"
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
    let output_directory = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is required"));
    let isolated_backend = output_directory.join("liborna_postgres18_backend_isolated.a");
    let archive_work_directory = output_directory.join("postgres-backend-symbols");
    prefix_backend_symbols(
        &output.join("liborna_postgres18_backend.a"),
        &isolated_backend,
        &archive_work_directory,
    );
    println!("cargo:rustc-link-search=native={}", output.display());
    println!(
        "cargo:rustc-link-search=native={}",
        output_directory.display()
    );
    println!("cargo:rustc-link-lib=static=orna_postgres18_initdb");
    println!("cargo:rustc-link-lib=static=orna_postgres18_backend_isolated");
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
