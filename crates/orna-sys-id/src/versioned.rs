//! Validated `name@version` identifiers for the open ABI namespace.
//!
//! Host roles, langitem tags and durable representation contracts are an open,
//! versioned namespace: the specification fixes the form, not the set (section 1.3 of
//! the sys-identity design). They therefore get a validating newtype rather than a
//! closed enum, so `@2.0` of an existing role stays expressible while malformed values
//! are refused at construction.

use core::fmt;

/// The closed set of ABI name prefixes the repository has used since 1.0.0.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Namespace {
    /// `host.*`: provider-side host operation roles.
    Host,
    /// `langitem.*`: language linkage tags.
    Langitem,
    /// `orna.*`: durable value and catalogue representation contracts.
    Orna,
    /// `std.*`: standard-library roles.
    Std,
    /// `sys.*`: system-surface roles.
    Sys,
    /// `vendor.*`: extension-defined roles.
    Vendor,
}

impl Namespace {
    /// Every namespace.
    pub const ALL: [Namespace; 6] = [
        Namespace::Host,
        Namespace::Langitem,
        Namespace::Orna,
        Namespace::Std,
        Namespace::Sys,
        Namespace::Vendor,
    ];

    /// The exact prefix without the separating dot.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Namespace::Host => "host",
            Namespace::Langitem => "langitem",
            Namespace::Orna => "orna",
            Namespace::Std => "std",
            Namespace::Sys => "sys",
            Namespace::Vendor => "vendor",
        }
    }

    /// The exact inverse of [`Self::as_str`].
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "host" => Some(Namespace::Host),
            "langitem" => Some(Namespace::Langitem),
            "orna" => Some(Namespace::Orna),
            "std" => Some(Namespace::Std),
            "sys" => Some(Namespace::Sys),
            "vendor" => Some(Namespace::Vendor),
            _ => None,
        }
    }
}

impl fmt::Display for Namespace {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A major version with an optional minor part.
///
/// `@1` and `@1.0` are distinct spellings in the tree today, and both must round-trip
/// byte-for-byte, so the minor component is optional rather than defaulted.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AbiVersion {
    major: u16,
    minor: Option<u16>,
}

impl AbiVersion {
    /// Constructs a version.
    #[must_use]
    pub const fn new(major: u16, minor: Option<u16>) -> Self {
        Self { major, minor }
    }

    /// The major component.
    #[must_use]
    pub const fn major(self) -> u16 {
        self.major
    }

    /// The minor component, absent when the spelling carried only a major part.
    #[must_use]
    pub const fn minor(self) -> Option<u16> {
        self.minor
    }
}

impl fmt::Display for AbiVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.minor {
            Some(minor) => write!(formatter, "{}.{minor}", self.major),
            None => write!(formatter, "{}", self.major),
        }
    }
}

/// A validated `namespace.name@major[.minor]` identifier.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct VersionedId {
    namespace: Namespace,
    path: Box<str>,
    version: AbiVersion,
}

impl VersionedId {
    /// Constructs an identifier, validating the path segments.
    pub fn new(namespace: Namespace, path: &str, version: AbiVersion) -> Result<Self, IdError> {
        validate_path(path)?;
        Ok(Self {
            namespace,
            path: path.into(),
            version,
        })
    }

    /// The namespace prefix.
    #[must_use]
    pub const fn namespace(&self) -> Namespace {
        self.namespace
    }

    /// The dotted path after the namespace, for example `std.io.fs.read`.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The version.
    #[must_use]
    pub const fn version(&self) -> AbiVersion {
        self.version
    }

    /// Whether two identifiers name the same role regardless of version.
    #[must_use]
    pub fn same_role(&self, other: &Self) -> bool {
        self.namespace == other.namespace && self.path == other.path
    }
}

impl fmt::Display for VersionedId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}.{}@{}",
            self.namespace, self.path, self.version
        )
    }
}

#[allow(clippy::cmp_owned)]
impl PartialEq<&str> for VersionedId {
    fn eq(&self, other: &&str) -> bool {
        self.to_string() == *other
    }
}

impl std::str::FromStr for VersionedId {
    type Err = IdError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (name, version) = text.split_once('@').ok_or(IdError::MissingVersion)?;
        if version.contains('@') {
            return Err(IdError::InvalidVersion {
                text: version.into(),
            });
        }
        let (namespace_text, path) = name.split_once('.').ok_or(IdError::MissingPath)?;
        let namespace =
            Namespace::parse(namespace_text).ok_or_else(|| IdError::UnknownNamespace {
                text: namespace_text.into(),
            })?;
        validate_path(path)?;
        let version = parse_version(version)?;
        Self::new(namespace, path, version)
    }
}

fn parse_version(text: &str) -> Result<AbiVersion, IdError> {
    if text.is_empty() {
        return Err(IdError::InvalidVersion { text: text.into() });
    }
    let mut parts = text.split('.');
    let major_text = parts.next().expect("split always yields one part");
    let minor_text = parts.next();
    if parts.next().is_some() {
        return Err(IdError::InvalidVersion { text: text.into() });
    }
    let major = parse_component(major_text, text)?;
    let minor = match minor_text {
        Some(minor) => Some(parse_component(minor, text)?),
        None => None,
    };
    Ok(AbiVersion::new(major, minor))
}

fn parse_component(component: &str, full: &str) -> Result<u16, IdError> {
    if component.is_empty()
        || !component.bytes().all(|byte| byte.is_ascii_digit())
        || (component.len() > 1 && component.starts_with('0'))
    {
        return Err(IdError::InvalidVersion { text: full.into() });
    }
    component
        .parse()
        .map_err(|_| IdError::VersionOverflow { text: full.into() })
}

fn validate_path(path: &str) -> Result<(), IdError> {
    if path.is_empty() {
        return Err(IdError::EmptyPath);
    }
    for segment in path.split('.') {
        let mut bytes = segment.bytes();
        let valid_start = bytes.next().is_some_and(|byte| byte.is_ascii_lowercase());
        let valid_rest =
            bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
        if !valid_start || !valid_rest {
            return Err(IdError::InvalidPathSegment {
                segment: segment.into(),
            });
        }
    }
    Ok(())
}

/// Why a `name@version` identifier was rejected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IdError {
    /// The text has no `@version` suffix.
    MissingVersion,
    /// The text has no `namespace.path` part.
    MissingPath,
    /// The prefix is not one of the closed ABI namespaces.
    UnknownNamespace {
        /// The rejected prefix.
        text: String,
    },
    /// The path is empty.
    EmptyPath,
    /// One path segment is empty or uses characters the ABI does not admit.
    InvalidPathSegment {
        /// The rejected segment.
        segment: String,
    },
    /// The version is not `major` or `major.minor` in canonical decimal form.
    InvalidVersion {
        /// The rejected version text.
        text: String,
    },
    /// A numeric component does not fit a `u16`.
    VersionOverflow {
        /// The rejected version text.
        text: String,
    },
}

impl fmt::Display for IdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdError::MissingVersion => formatter.write_str("versioned id is missing `@version`"),
            IdError::MissingPath => formatter.write_str("versioned id is missing `namespace.path`"),
            IdError::UnknownNamespace { text } => {
                write!(formatter, "unknown ABI namespace `{text}`")
            }
            IdError::EmptyPath => formatter.write_str("versioned id path is empty"),
            IdError::InvalidPathSegment { segment } => {
                write!(formatter, "invalid versioned id path segment `{segment}`")
            }
            IdError::InvalidVersion { text } => {
                write!(formatter, "invalid ABI version `{text}`")
            }
            IdError::VersionOverflow { text } => {
                write!(formatter, "ABI version `{text}` does not fit a u16")
            }
        }
    }
}

impl std::error::Error for IdError {}
