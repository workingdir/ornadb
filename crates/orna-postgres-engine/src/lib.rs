#![cfg_attr(not(feature = "embedded"), allow(dead_code))]

#[cfg(feature = "embedded")]
mod embedded {
    use std::{
        ffi::{CString, OsStr},
        fmt,
        os::{raw::c_char, unix::ffi::OsStrExt},
        path::Path,
    };

    unsafe extern "C" {
        fn orna_postgres18_entry(argc: i32, argv: *mut *mut c_char) -> i32;
        fn orna_postgres18_initdb_entry(data_directory: *const c_char) -> i32;
        fn orna_postgres18_set_support_root(absolute_root: *const c_char) -> i32;
        fn orna_postgres18_read_control(
            data_directory: *const c_char,
            control: *mut RawControlData,
        ) -> i32;
    }

    #[derive(Clone, Copy)]
    #[repr(C)]
    struct RawControlData {
        system_identifier: u64,
        pg_control_version: u32,
        catalog_version: u32,
        state: u32,
        data_checksum_version: u32,
    }

    /// A rejected typed input to the embedded PostgreSQL C boundary.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum EngineError {
        /// A path is relative, too long, or contains a NUL byte.
        InvalidAbsolutePath,
        /// An entry argument is empty, invalid, or contains a NUL byte.
        InvalidArgument,
        /// The linked engine did not accept the process-local support root.
        SupportRootRejected,
        /// The linked engine could not read or validate the PostgreSQL control file.
        ControlDataRejected,
    }

    impl fmt::Display for EngineError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(match self {
                Self::InvalidAbsolutePath => "embedded PostgreSQL path is not an absolute C string",
                Self::InvalidArgument => "embedded PostgreSQL argument is not a C string",
                Self::SupportRootRejected => "embedded PostgreSQL support root was rejected",
                Self::ControlDataRejected => "embedded PostgreSQL control data was rejected",
            })
        }
    }

    impl std::error::Error for EngineError {}

    /// An owned absolute path that is safe to pass to a linked PostgreSQL entry.
    #[derive(Debug, Clone)]
    pub struct AbsolutePath(CString);

    impl AbsolutePath {
        /// Validates and owns one absolute path for a linked C entry.
        pub fn new(path: &Path) -> Result<Self, EngineError> {
            let bytes = path.as_os_str().as_bytes();
            if !path.is_absolute() || bytes.len() >= 4096 {
                return Err(EngineError::InvalidAbsolutePath);
            }
            CString::new(bytes)
                .map(Self)
                .map_err(|_| EngineError::InvalidAbsolutePath)
        }

        fn as_ptr(&self) -> *const c_char {
            self.0.as_ptr()
        }
    }

    /// Writable, contiguous argument storage retained for one linked PostgreSQL entry.
    #[derive(Debug)]
    pub struct LinkedArguments {
        storage: Vec<u8>,
        pointers: Vec<*mut c_char>,
    }

    impl LinkedArguments {
        /// Copies an argument vector into one bounded, contiguous writable buffer.
        pub fn new<I, S>(arguments: I) -> Result<Self, EngineError>
        where
            I: IntoIterator<Item = S>,
            S: AsRef<OsStr>,
        {
            let values = arguments
                .into_iter()
                .map(|value| {
                    CString::new(value.as_ref().as_bytes())
                        .map_err(|_| EngineError::InvalidArgument)
                })
                .collect::<Result<Vec<_>, _>>()?;
            if values.is_empty() || values.len() > i32::MAX as usize {
                return Err(EngineError::InvalidArgument);
            }
            let capacity = values
                .iter()
                .map(|value| value.as_bytes_with_nul().len())
                .sum();
            let mut storage = Vec::with_capacity(capacity);
            let mut offsets = Vec::with_capacity(values.len());
            for value in &values {
                offsets.push(storage.len());
                storage.extend_from_slice(value.as_bytes_with_nul());
            }
            let base = storage.as_mut_ptr();
            let mut pointers = offsets
                .into_iter()
                .map(|offset| base.wrapping_add(offset).cast())
                .collect::<Vec<_>>();
            pointers.push(std::ptr::null_mut());
            Ok(Self { storage, pointers })
        }

        fn raw_parts(&mut self) -> (i32, *mut *mut c_char) {
            debug_assert!(!self.storage.is_empty());
            ((self.pointers.len() - 1) as i32, self.pointers.as_mut_ptr())
        }
    }

    /// A configured process-local handle to the linked PostgreSQL engine.
    #[derive(Debug)]
    pub struct EmbeddedEngine {
        _private: (),
    }

    /// The typed immutable facts read from one PostgreSQL control file.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct ControlData {
        system_identifier: u64,
        pg_control_version: u32,
        catalog_version: u32,
        state: u32,
        data_checksum_version: u32,
    }

    impl ControlData {
        /// Returns the cluster's unique PostgreSQL system identifier.
        pub const fn system_identifier(self) -> u64 {
            self.system_identifier
        }

        /// Returns the PostgreSQL control-file format version.
        pub const fn pg_control_version(self) -> u32 {
            self.pg_control_version
        }

        /// Returns the PostgreSQL catalogue format version.
        pub const fn catalog_version(self) -> u32 {
            self.catalog_version
        }

        /// Returns the PostgreSQL database-state value.
        pub const fn state(self) -> u32 {
            self.state
        }

        /// Returns the data-page checksum format version.
        pub const fn data_checksum_version(self) -> u32 {
            self.data_checksum_version
        }
    }

    impl EmbeddedEngine {
        /// Configures the linked engine's one-shot process-global support root.
        ///
        /// # Safety
        ///
        /// The caller must be a fresh, single-threaded child process that has not entered
        /// PostgreSQL and will not call this concurrently.
        pub unsafe fn configure_process(support_root: &AbsolutePath) -> Result<Self, EngineError> {
            // SAFETY: AbsolutePath owns a live, NUL-terminated C string for this call.
            let result = unsafe { orna_postgres18_set_support_root(support_root.as_ptr()) };
            if result == 0 {
                Ok(Self { _private: () })
            } else {
                Err(EngineError::SupportRootRejected)
            }
        }

        /// Calls the linked initialiser. The caller must be a fresh, single-threaded child process.
        ///
        /// # Safety
        ///
        /// PostgreSQL owns process-global state and may terminate the calling process.
        pub unsafe fn initialise_process(&self, data_directory: &AbsolutePath) -> i32 {
            // SAFETY: upheld by the caller; the path remains live for the complete call.
            unsafe { orna_postgres18_initdb_entry(data_directory.as_ptr()) }
        }

        /// Calls one linked PostgreSQL role with writable, contiguous argument storage.
        ///
        /// # Safety
        ///
        /// The caller must be a fresh, single-threaded child prepared for PostgreSQL to own or
        /// terminate the process. `arguments` remains live and writable for the complete call.
        pub unsafe fn run_process(&self, arguments: &mut LinkedArguments) -> i32 {
            let (count, pointers) = arguments.raw_parts();
            // SAFETY: upheld by the caller and by LinkedArguments' stable owned buffers.
            unsafe { orna_postgres18_entry(count, pointers) }
        }

        /// Reads and validates one stopped cluster's PostgreSQL control file.
        pub fn read_control(
            &self,
            data_directory: &AbsolutePath,
        ) -> Result<ControlData, EngineError> {
            let mut raw = RawControlData {
                system_identifier: 0,
                pg_control_version: 0,
                catalog_version: 0,
                state: 0,
                data_checksum_version: 0,
            };
            // SAFETY: both owned pointers remain live for the complete read-only call.
            if unsafe { orna_postgres18_read_control(data_directory.as_ptr(), &mut raw) } != 0 {
                return Err(EngineError::ControlDataRejected);
            }
            Ok(ControlData {
                system_identifier: raw.system_identifier,
                pg_control_version: raw.pg_control_version,
                catalog_version: raw.catalog_version,
                state: raw.state,
                data_checksum_version: raw.data_checksum_version,
            })
        }
    }

    /// The verified data-only PostgreSQL support archive embedded in Orna.
    pub const SUPPORT_ARCHIVE: &[u8] = include_bytes!(env!("ORNA_POSTGRES_SUPPORT_BUNDLE"));
    /// The exhaustive member manifest for [`SUPPORT_ARCHIVE`].
    pub const SUPPORT_MANIFEST: &[u8] = include_bytes!(env!("ORNA_POSTGRES_SUPPORT_MANIFEST"));
    /// The build evidence that identifies the linked PostgreSQL engine.
    pub const ENGINE_MANIFEST: &[u8] = include_bytes!(env!("ORNA_POSTGRES_ENGINE_MANIFEST"));
    /// The PostgreSQL licence bytes shipped with the embedded engine.
    pub const POSTGRESQL_LICENCE: &[u8] = include_bytes!(env!("ORNA_POSTGRES_LICENSE"));

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn validates_absolute_paths() {
            assert!(AbsolutePath::new(Path::new("/var/lib/orna")).is_ok());
            assert_eq!(
                AbsolutePath::new(Path::new("relative")).unwrap_err(),
                EngineError::InvalidAbsolutePath
            );
        }

        #[test]
        fn creates_writable_contiguous_arguments() {
            let mut arguments =
                LinkedArguments::new(["/usr/bin/orna", "--describe-config"]).unwrap();
            let (count, pointers) = arguments.raw_parts();
            assert_eq!(count, 2);
            // SAFETY: raw_parts points into the live argument buffer.
            unsafe {
                assert_eq!(
                    std::ffi::CStr::from_ptr(*pointers).to_bytes(),
                    b"/usr/bin/orna"
                );
                assert_eq!(
                    std::ffi::CStr::from_ptr(*pointers.add(1)).to_bytes(),
                    b"--describe-config"
                );
                assert!((*pointers.add(2)).is_null());
            }
        }

        #[test]
        fn rejects_missing_control_data() {
            let engine = EmbeddedEngine { _private: () };
            let data_directory = AbsolutePath::new(Path::new("/missing-orna-pgdata")).unwrap();
            let error = engine.read_control(&data_directory).unwrap_err();
            assert_eq!(error, EngineError::ControlDataRejected);
        }
    }
}

#[cfg(feature = "embedded")]
pub use embedded::*;
