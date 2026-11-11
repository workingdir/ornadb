//! The only module of this crate that contains `unsafe` code. Each block states
//! the invariant it relies on. Nothing here decides policy: the digest check and
//! the ABI comparison happen in the safe modules, and this module only performs
//! the calls.

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::path::{Path, PathBuf};

use libloading::Library;

/// A library loaded into the process. Dropping it unloads the library.
pub(crate) struct Loaded {
    library: Library,
}

/// Creates an anonymous memory file holding `bytes`. Returns the open file and the
/// `/proc/self/fd/<n>` path that `dlopen` can load.
#[cfg(target_os = "linux")]
pub(crate) fn memfd(bytes: &[u8]) -> std::io::Result<(std::fs::File, PathBuf)> {
    use std::ffi::CStr;
    use std::io::Write;
    use std::os::fd::FromRawFd;

    const NAME: &CStr = c"orna-native";
    // SAFETY: NAME is a NUL-terminated string that outlives the call, and
    // MFD_CLOEXEC is a flag value libc defines for memfd_create.
    let fd = unsafe { libc::memfd_create(NAME.as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: fd is a descriptor memfd_create just returned, so no other owner
    // holds it; File takes ownership and closes it on drop.
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    file.write_all(bytes)?;
    Ok((file, PathBuf::from(format!("/proc/self/fd/{fd}"))))
}

impl Loaded {
    /// Loads the library at `path`, which the caller has verified and staged.
    ///
    /// # Errors
    ///
    /// Returns the loader's message when the platform refuses the file.
    pub(crate) fn open(path: &Path) -> Result<Self, String> {
        // SAFETY: `path` is the private copy whose bytes the caller just checked
        // against the pinned digest. Loading runs the library's initialisers; the
        // design treats a verified library as trusted host code (native-libraries.md
        // section 8).
        let library = unsafe { Library::new(path) }.map_err(|error| error.to_string())?;
        Ok(Self { library })
    }

    /// The number `orna_library_abi` returns, or `None` when the export is missing.
    pub(crate) fn abi(&self) -> Option<u32> {
        // SAFETY: the export is declared `uint32_t orna_library_abi(void)` by the
        // library ABI (native-libraries.md section 4). The digest check ties this
        // symbol to the bytes the caller verified.
        let function = unsafe {
            self.library
                .get::<unsafe extern "C" fn() -> u32>(b"orna_library_abi")
        }
        .ok()?;
        let call: unsafe extern "C" fn() -> u32 = *function;
        // SAFETY: `call` is the export with the signature above and takes no arguments.
        Some(unsafe { call() })
    }

    /// The address of the export `name`, or `None` when the library does not export it.
    /// The address is only returned, never called here.
    pub(crate) fn symbol_address(&self, name: &str) -> Option<*const c_void> {
        // SAFETY: the symbol is read only as an address. No call is made through
        // it here, so the declared type does not have to match the export.
        let symbol = unsafe { self.library.get::<unsafe extern "C" fn()>(name.as_bytes()) }.ok()?;
        let function: unsafe extern "C" fn() = *symbol;
        Some(function as *const c_void)
    }
}
