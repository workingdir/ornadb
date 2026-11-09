//! Emits the typed host-declaration vocabulary used by `build_host`.
//!
//! The build script validates declaration attributes against the generated
//! identity crate. `orna-sys-id` is a build dependency only: the shipped
//! `orna-sys-codegen` library keeps no runtime edge on the identity crate, so the
//! crate-layer table is unchanged.

use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let mut out = String::new();
    out.push_str("// Generated typed sys vocabulary for host-declaration parsing.\n");
    out.push_str("pub(crate) const SYS_FUNCTION_VARIANTS: &[(&str, &str)] = &[\n");
    for function in orna_sys_id::SysFunction::ALL {
        out.push_str(&format!(
            "    (\"{}\", \"{}\"),\n",
            function.variant_name(),
            function.label()
        ));
    }
    out.push_str("];\n\n");
    out.push_str("pub(crate) const SYS_FAILURE_CODE_VARIANTS: &[(&str, &str)] = &[\n");
    for code in orna_sys_id::SysFailureCode::ALL {
        out.push_str(&format!(
            "    (\"{}\", \"{}\"),\n",
            code.variant_name(),
            code.as_str()
        ));
    }
    out.push_str("];\n\n");
    out.push_str("pub(crate) const HOST_OPERATION_VARIANTS: &[(&str, &str, &str, &str)] = &[\n");
    for operation in orna_sys_id::HostOperation::ALL {
        out.push_str(&format!(
            "    (\"{}\", \"{}\", \"{}\", \"{}\"),\n",
            operation.variant_name(),
            operation.label(),
            operation.role(),
            operation.provider().name()
        ));
    }
    out.push_str("];\n\n");
    out.push_str("pub(crate) const HOST_PROVIDER_VARIANTS: &[(&str, &str)] = &[\n");
    for provider in orna_sys_id::HostProvider::ALL {
        out.push_str(&format!(
            "    (\"{}\", \"{}\"),\n",
            provider.variant_name(),
            provider.name()
        ));
    }
    out.push_str("];\n");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    fs::write(out_dir.join("sys_host_vocabulary.rs"), out)
        .expect("write typed host vocabulary");
}
