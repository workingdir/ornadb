//! Link errors: one code per cause, each naming the declaration and the library.

use std::fmt;

use orna_syntax::SyntaxSpan;

/// The stable code of one link failure (docs/design/native-libraries.md, section 11).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkErrorCode {
    /// No registry entry and no library serves the declaration.
    Unbound,
    /// Two libraries list one module, or two declarations share a native key.
    Duplicate,
    /// A library has no entry for this target, its file is missing or outside the
    /// project, or a path is malformed.
    Library,
    /// The library file's SHA-256 differs from the manifest digest.
    Digest,
    /// An ABI number differs from the one this engine implements.
    Abi,
    /// A declared symbol is not exported by the library. The loader raises it
    /// (slice 2b); the plan does not.
    Symbol,
    /// The link profile cannot bind the declaration.
    Profile,
    /// The project's native-library manifest is malformed. Not in the design's
    /// code table; see the crate documentation.
    Manifest,
}

impl LinkErrorCode {
    /// The code printed in diagnostics.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Unbound => "ORNA-STD-NATIVE-UNBOUND",
            Self::Duplicate => "ORNA-STD-NATIVE-DUPLICATE",
            Self::Library => "ORNA-STD-NATIVE-LIBRARY",
            Self::Digest => "ORNA-STD-NATIVE-DIGEST",
            Self::Abi => "ORNA-STD-NATIVE-ABI",
            Self::Symbol => "ORNA-STD-NATIVE-SYMBOL",
            Self::Profile => "ORNA-STD-NATIVE-PROFILE",
            Self::Manifest => "ORNA-STD-NATIVE-MANIFEST",
        }
    }
}

/// One native declaration named in a link error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclSite {
    /// The declaring module path, dotted.
    pub module: String,
    /// The declared name.
    pub name: String,
    /// `file:line` of the declaration, or `unlocated` when the span has no location.
    pub location: String,
}

impl DeclSite {
    /// Names the declaration at `span`.
    #[must_use]
    pub fn new(module: String, name: String, span: &SyntaxSpan) -> Self {
        let location = match &span.location {
            Some(location) => format!("{}:{}", location.file, location.start.line),
            None => "unlocated".to_owned(),
        };
        Self {
            module,
            name,
            location,
        }
    }

    fn describe(&self) -> String {
        if self.module.is_empty() {
            format!("{} ({})", self.name, self.location)
        } else {
            format!("{}.{} ({})", self.module, self.name, self.location)
        }
    }
}

/// One link failure. Every field after `code` says what it names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkError {
    /// The failure's code.
    pub code: LinkErrorCode,
    /// The declarations the failure affects. Empty for manifest errors.
    pub declarations: Vec<DeclSite>,
    /// The library the failure concerns, when there is one.
    pub library: Option<String>,
    /// What went wrong, with the expected and found values where there are two.
    pub detail: String,
}

impl LinkError {
    /// A failure with no declaration and no library yet named.
    #[must_use]
    pub fn new(code: LinkErrorCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            declarations: Vec::new(),
            library: None,
            detail: detail.into(),
        }
    }

    /// Names the library the failure concerns.
    #[must_use]
    pub fn with_library(mut self, library: &str) -> Self {
        self.library = Some(library.to_owned());
        self
    }

    /// Names the declarations the failure affects.
    #[must_use]
    pub fn with_declarations(mut self, declarations: Vec<DeclSite>) -> Self {
        self.declarations = declarations;
        self
    }
}

impl fmt::Display for LinkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "error[{}]: {}", self.code.code(), self.detail)?;
        if let Some(library) = &self.library {
            write!(formatter, "\n  = library: {library}")?;
        }
        for declaration in &self.declarations {
            write!(formatter, "\n  = declares: {}", declaration.describe())?;
        }
        Ok(())
    }
}

impl std::error::Error for LinkError {}
