//! Closed local capability grants for CLIENT evaluation (ADR 0060).
//!
//! The local `orna` client holds a [`LocalCapabilityGrantSet`] derived from
//! local configuration — never from the database. The invocation-time
//! enforcement gate (ADR 0060 step 6) admits a CLIENT function revision only
//! when every declared capability is granted, with the declared argument
//! scope checked against the grant's scope where the vocabulary defines
//! scope.
//!
//! The capability vocabulary is closed to four entries:
//!
//! | Capability | Argument shape |
//! | --- | --- |
//! | `std.fs.read` | one path-scope argument |
//! | `std.fs.write` | one path-scope argument |
//! | `std.net.connect` | one host-scope argument |
//! | `std.secret.use` | one secret-id argument |
//!
//! Scope compatibility rule: a path grant covers a resolved argument path
//! exactly or under it (component-wise prefix with a boundary — `/home/bob`
//! covers `/home/bob` and `/home/bob/x` but not `/home/bobette`), after
//! lexical `.`/`..` normalisation and with no symlink resolution. Host and
//! secret-id grants require an exact match. A relative or unresolved argument
//! fails closed.

use std::{
    error::Error,
    fmt,
    path::{Component, Path, PathBuf},
    str::FromStr,
};

/// The closed CLIENT capability vocabulary (ADR 0060).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum LocalCapabilityName {
    /// `std.fs.read` — read access to a resolved path scope.
    StdFsRead,
    /// `std.fs.write` — write access to a resolved path scope.
    StdFsWrite,
    /// `std.net.connect` — connect to a resolved host scope.
    StdNetConnect,
    /// `std.secret.use` — use the named secret.
    StdSecretUse,
}

impl LocalCapabilityName {
    /// Every vocabulary entry in closed order.
    pub const ALL: [LocalCapabilityName; 4] = [
        LocalCapabilityName::StdFsRead,
        LocalCapabilityName::StdFsWrite,
        LocalCapabilityName::StdNetConnect,
        LocalCapabilityName::StdSecretUse,
    ];

    /// The qualified capability name as written in the vocabulary.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StdFsRead => "std.fs.read",
            Self::StdFsWrite => "std.fs.write",
            Self::StdNetConnect => "std.net.connect",
            Self::StdSecretUse => "std.secret.use",
        }
    }

    /// Parses one exact vocabulary name.
    ///
    /// The match is exact and case-sensitive; anything else is
    /// [`LocalCapabilityGrantError::UnknownCapability`].
    pub fn parse(text: &str) -> Result<Self, LocalCapabilityGrantError> {
        match text {
            "std.fs.read" => Ok(Self::StdFsRead),
            "std.fs.write" => Ok(Self::StdFsWrite),
            "std.net.connect" => Ok(Self::StdNetConnect),
            "std.secret.use" => Ok(Self::StdSecretUse),
            _ => Err(LocalCapabilityGrantError::UnknownCapability {
                name: text.to_owned(),
            }),
        }
    }
}

impl fmt::Display for LocalCapabilityName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for LocalCapabilityName {
    type Err = LocalCapabilityGrantError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}

/// An absolute filesystem path scope.
///
/// Constructed from an absolute path, lexically normalised (`.` removed,
/// `..` resolved) and stored without symlink resolution.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct LocalPathScope {
    path: PathBuf,
}

impl LocalPathScope {
    /// Constructs a path scope, rejecting empty and relative paths.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, LocalCapabilityGrantError> {
        let path = path.into();
        if path.as_os_str().is_empty() {
            return Err(LocalCapabilityGrantError::EmptyScope);
        }
        if !path.is_absolute() {
            return Err(LocalCapabilityGrantError::InvalidScope {
                detail: format!("path scope must be absolute, got `{}`", path.display()),
            });
        }
        Ok(Self {
            path: normalise_path(&path),
        })
    }

    /// Returns the normalised absolute path.
    pub fn as_path(&self) -> &Path {
        &self.path
    }
}

impl fmt::Display for LocalPathScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.path.display().fmt(formatter)
    }
}

/// A host scope: a hostname, IP address, or `host:port`.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct LocalHostScope {
    host: String,
}

impl LocalHostScope {
    /// Constructs a host scope, rejecting empty and whitespace-containing hosts.
    pub fn new(host: impl Into<String>) -> Result<Self, LocalCapabilityGrantError> {
        let host = host.into();
        if host.trim().is_empty() {
            return Err(LocalCapabilityGrantError::EmptyScope);
        }
        if host != host.trim() {
            return Err(LocalCapabilityGrantError::InvalidScope {
                detail: format!(
                    "host scope must not have leading or trailing whitespace, got `{host}`"
                ),
            });
        }
        if host.chars().any(char::is_whitespace) {
            return Err(LocalCapabilityGrantError::InvalidScope {
                detail: format!("host scope must not contain whitespace, got `{host}`"),
            });
        }
        Ok(Self { host })
    }

    /// Returns the exact host scope text.
    pub fn as_str(&self) -> &str {
        &self.host
    }
}

impl fmt::Display for LocalHostScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.host)
    }
}

/// A secret identifier.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct LocalSecretId {
    id: String,
}

impl LocalSecretId {
    /// Constructs a secret id, rejecting empty and outer-whitespace ids.
    pub fn new(id: impl Into<String>) -> Result<Self, LocalCapabilityGrantError> {
        let id = id.into();
        if id.trim().is_empty() {
            return Err(LocalCapabilityGrantError::EmptyScope);
        }
        if id != id.trim() {
            return Err(LocalCapabilityGrantError::InvalidScope {
                detail: format!(
                    "secret id must not have leading or trailing whitespace, got `{id}`"
                ),
            });
        }
        Ok(Self { id })
    }

    /// Returns the exact secret id text.
    pub fn as_str(&self) -> &str {
        &self.id
    }
}

impl fmt::Display for LocalSecretId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.id)
    }
}

/// The closed typed scope value of one local capability grant.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum LocalCapabilityScope {
    /// An absolute filesystem path scope.
    Path(LocalPathScope),
    /// A host scope (hostname, IP address, or `host:port`).
    Host(LocalHostScope),
    /// A secret identifier.
    Secret(LocalSecretId),
}

impl LocalCapabilityScope {
    /// Constructs a path scope.
    pub fn path(path: impl Into<PathBuf>) -> Result<Self, LocalCapabilityGrantError> {
        Ok(Self::Path(LocalPathScope::new(path)?))
    }

    /// Constructs a host scope.
    pub fn host(host: impl Into<String>) -> Result<Self, LocalCapabilityGrantError> {
        Ok(Self::Host(LocalHostScope::new(host)?))
    }

    /// Constructs a secret-id scope.
    pub fn secret(id: impl Into<String>) -> Result<Self, LocalCapabilityGrantError> {
        Ok(Self::Secret(LocalSecretId::new(id)?))
    }

    /// Returns true when this scope covers the resolved argument value.
    ///
    /// Path scopes use the component-wise prefix-with-boundary rule after
    /// lexical normalisation; host and secret-id scopes require an exact
    /// match. A relative path value is never covered.
    pub fn satisfies(&self, value: &str) -> bool {
        match self {
            Self::Path(scope) => path_satisfies(scope.as_path(), value),
            Self::Host(scope) => scope.as_str() == value,
            Self::Secret(scope) => scope.as_str() == value,
        }
    }
}

impl fmt::Display for LocalCapabilityScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Path(scope) => scope.fmt(formatter),
            Self::Host(scope) => scope.fmt(formatter),
            Self::Secret(scope) => scope.fmt(formatter),
        }
    }
}

/// One local capability grant: a closed capability name plus its typed scope.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct LocalCapabilityGrant {
    name: LocalCapabilityName,
    scope: LocalCapabilityScope,
}

impl LocalCapabilityGrant {
    /// Constructs a grant, rejecting a scope incompatible with the capability.
    ///
    /// `std.fs.read` and `std.fs.write` require a path scope,
    /// `std.net.connect` a host scope, and `std.secret.use` a secret-id scope.
    pub fn new(
        name: LocalCapabilityName,
        scope: LocalCapabilityScope,
    ) -> Result<Self, LocalCapabilityGrantError> {
        let expected = match name {
            LocalCapabilityName::StdFsRead | LocalCapabilityName::StdFsWrite => "path-scope",
            LocalCapabilityName::StdNetConnect => "host-scope",
            LocalCapabilityName::StdSecretUse => "secret-id",
        };
        let actual = match &scope {
            LocalCapabilityScope::Path(_) => "path-scope",
            LocalCapabilityScope::Host(_) => "host-scope",
            LocalCapabilityScope::Secret(_) => "secret-id",
        };
        if expected != actual {
            return Err(LocalCapabilityGrantError::InvalidScope {
                detail: format!(
                    "capability {name} requires a {expected} argument but the grant scope is a {actual}"
                ),
            });
        }
        Ok(Self { name, scope })
    }

    /// Returns the granted capability name.
    pub const fn name(&self) -> LocalCapabilityName {
        self.name
    }

    /// Returns the granted scope.
    pub fn scope(&self) -> &LocalCapabilityScope {
        &self.scope
    }
}

impl fmt::Display for LocalCapabilityGrant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}({})", self.name, self.scope)
    }
}

/// The argument source of one declared capability requirement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalCapabilityArgumentSource {
    /// A literal scope value written in the declaration (unquoted).
    Text(String),
    /// A reference to a declared function parameter.
    Parameter(String),
}

/// A declared capability requirement, mirroring the syntax
/// `CapabilitySpecification` form (qualified name plus one argument source).
///
/// The enforcement gate constructs this from the function revision's declared
/// requirement and resolves parameter references to invocation values before
/// asking the grant set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalCapabilityDeclaration {
    name: LocalCapabilityName,
    argument: LocalCapabilityArgumentSource,
}

impl LocalCapabilityDeclaration {
    /// Constructs the declaration from a checked capability name and argument source.
    pub fn new(name: LocalCapabilityName, argument: LocalCapabilityArgumentSource) -> Self {
        Self { name, argument }
    }

    /// Returns the declared capability name.
    pub const fn name(&self) -> LocalCapabilityName {
        self.name
    }

    /// Returns the declared argument source.
    pub fn argument(&self) -> &LocalCapabilityArgumentSource {
        &self.argument
    }
}

/// A closed construction/validation failure for local capability grants.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalCapabilityGrantError {
    /// The capability name is outside the closed vocabulary.
    UnknownCapability {
        /// The offending name text.
        name: String,
    },
    /// A scope value was empty or whitespace-only.
    EmptyScope,
    /// A scope value is malformed or incompatible with its capability.
    InvalidScope {
        /// The human-readable reason.
        detail: String,
    },
    /// The grant set already contains an identical grant.
    DuplicateGrant {
        /// The duplicated grant.
        grant: LocalCapabilityGrant,
    },
}

impl fmt::Display for LocalCapabilityGrantError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownCapability { name } => {
                write!(formatter, "unknown local capability `{name}`")
            }
            Self::EmptyScope => formatter.write_str("local capability scope must not be empty"),
            Self::InvalidScope { detail } => formatter.write_str(detail),
            Self::DuplicateGrant { grant } => {
                write!(formatter, "duplicate local capability grant {grant}")
            }
        }
    }
}

impl Error for LocalCapabilityGrantError {}

/// An immutable, ordered, deduplicated set of local capability grants.
///
/// Grants are kept in first-occurrence order. Two grants are duplicates when
/// their capability name and scope are equal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalCapabilityGrantSet {
    grants: Vec<LocalCapabilityGrant>,
}

impl LocalCapabilityGrantSet {
    /// Returns the empty grant set, which denies every capability.
    pub const fn new() -> Self {
        Self { grants: Vec::new() }
    }

    /// Constructs the set from grants, preserving first-occurrence order.
    ///
    /// Fails closed on the first duplicate grant; a failed construction
    /// returns no set.
    pub fn from_grants(
        grants: impl IntoIterator<Item = LocalCapabilityGrant>,
    ) -> Result<Self, LocalCapabilityGrantError> {
        let mut set = Vec::new();
        for grant in grants {
            if set.contains(&grant) {
                return Err(LocalCapabilityGrantError::DuplicateGrant { grant });
            }
            set.push(grant);
        }
        Ok(Self { grants: set })
    }

    /// Returns true when the set holds at least one grant for the capability.
    pub fn contains(&self, name: LocalCapabilityName) -> bool {
        self.grants.iter().any(|grant| grant.name() == name)
    }

    /// Returns true when a grant for `name` covers the resolved argument value.
    ///
    /// Applies the scope-compatibility rule: component-wise path prefix for
    /// path scopes, exact match for host and secret-id scopes. Uncovered and
    /// unresolved values fail closed.
    pub fn satisfies(&self, name: LocalCapabilityName, resolved_argument: &str) -> bool {
        self.grants
            .iter()
            .any(|grant| grant.name() == name && grant.scope().satisfies(resolved_argument))
    }

    /// Returns true when the set satisfies a declared capability requirement.
    ///
    /// A literal declaration argument is used directly; a parameter
    /// declaration argument is resolved through `resolve`, which binds the
    /// parameter name to its invocation value. A parameter that cannot be
    /// resolved fails closed. Parameter resolution happens at the gate; this
    /// helper is pure.
    pub fn satisfies_declaration(
        &self,
        declaration: &LocalCapabilityDeclaration,
        resolve: impl FnOnce(&str) -> Option<String>,
    ) -> bool {
        let value = match declaration.argument() {
            LocalCapabilityArgumentSource::Text(text) => text.clone(),
            LocalCapabilityArgumentSource::Parameter(parameter) => match resolve(parameter) {
                Some(value) => value,
                None => return false,
            },
        };
        self.satisfies(declaration.name(), &value)
    }

    /// Returns the grants in first-occurrence order.
    pub fn as_slice(&self) -> &[LocalCapabilityGrant] {
        &self.grants
    }

    /// Returns the number of grants in the set.
    pub fn len(&self) -> usize {
        self.grants.len()
    }

    /// Returns true when the set contains no grants.
    pub fn is_empty(&self) -> bool {
        self.grants.is_empty()
    }
}

impl Default for LocalCapabilityGrantSet {
    fn default() -> Self {
        Self::new()
    }
}

/// Returns the path with `.` removed and `..` resolved lexically.
///
/// A `..` after the root is dropped (it cannot escape the root), and a `..`
/// with no normal component to pop is dropped.
fn normalise_path(path: &Path) -> PathBuf {
    let mut normalised = PathBuf::new();
    for component in normalise_components(path) {
        normalised.push(component.as_os_str());
    }
    normalised
}

/// Returns the normalised component list of `path`.
fn normalise_components(path: &Path) -> Vec<Component<'_>> {
    let mut components: Vec<Component<'_>> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if let Some(Component::Normal(_)) = components.last() {
                    components.pop();
                }
            }
            other => components.push(other),
        }
    }
    components
}

/// True when the normalised absolute `value` is exactly or under `grant`.
fn path_satisfies(grant: &Path, value: &str) -> bool {
    let value = Path::new(value);
    if !value.is_absolute() {
        return false;
    }
    let grant = normalise_components(grant);
    let value = normalise_components(value);
    value.len() >= grant.len()
        && grant
            .iter()
            .zip(value.iter())
            .all(|(grant_component, value_component)| grant_component == value_component)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(name: &str, scope: LocalCapabilityScope) -> LocalCapabilityGrant {
        LocalCapabilityGrant::new(LocalCapabilityName::parse(name).unwrap(), scope).unwrap()
    }

    fn path_scope(path: &str) -> LocalCapabilityScope {
        LocalCapabilityScope::path(path).unwrap()
    }

    fn host_scope(host: &str) -> LocalCapabilityScope {
        LocalCapabilityScope::host(host).unwrap()
    }

    fn secret_scope(id: &str) -> LocalCapabilityScope {
        LocalCapabilityScope::secret(id).unwrap()
    }

    #[test]
    fn parses_the_closed_capability_vocabulary() {
        for (name, text) in [
            (LocalCapabilityName::StdFsRead, "std.fs.read"),
            (LocalCapabilityName::StdFsWrite, "std.fs.write"),
            (LocalCapabilityName::StdNetConnect, "std.net.connect"),
            (LocalCapabilityName::StdSecretUse, "std.secret.use"),
        ] {
            assert_eq!(name.as_str(), text);
            assert_eq!(LocalCapabilityName::parse(text), Ok(name));
            assert_eq!(text.parse::<LocalCapabilityName>(), Ok(name));
            assert!(LocalCapabilityName::ALL.contains(&name));
        }
    }

    #[test]
    fn rejects_names_outside_the_closed_vocabulary() {
        for unknown in [
            "std.fs.call",
            "std.net.listen",
            "STD.FS.READ",
            "std.fs.read ",
            "std.fs",
            "",
        ] {
            let error = LocalCapabilityName::parse(unknown).unwrap_err();
            assert_eq!(
                error,
                LocalCapabilityGrantError::UnknownCapability {
                    name: unknown.to_owned()
                },
                "name: {unknown}"
            );
        }
    }

    #[test]
    fn constructs_a_grant_for_every_vocabulary_entry() {
        let read = grant("std.fs.read", path_scope("/home/bob"));
        assert_eq!(read.name(), LocalCapabilityName::StdFsRead);
        assert!(read.scope().satisfies("/home/bob/x"));

        let write = grant("std.fs.write", path_scope("/tmp"));
        assert_eq!(write.name(), LocalCapabilityName::StdFsWrite);

        let connect = grant("std.net.connect", host_scope("db.internal"));
        assert_eq!(connect.name(), LocalCapabilityName::StdNetConnect);

        let secret = grant("std.secret.use", secret_scope("payments-key"));
        assert_eq!(secret.name(), LocalCapabilityName::StdSecretUse);
    }

    #[test]
    fn rejects_a_scope_kind_mismatched_with_its_capability() {
        for (name, scope) in [
            ("std.fs.read", host_scope("db.internal")),
            ("std.fs.write", secret_scope("key")),
            ("std.net.connect", path_scope("/home/bob")),
            ("std.secret.use", host_scope("db.internal")),
        ] {
            let error = LocalCapabilityGrant::new(LocalCapabilityName::parse(name).unwrap(), scope)
                .unwrap_err();
            assert!(
                matches!(error, LocalCapabilityGrantError::InvalidScope { .. }),
                "{name}: {error}"
            );
            assert!(error.to_string().contains("requires a"), "{name}: {error}");
        }
    }

    #[test]
    fn rejects_empty_and_whitespace_only_scopes() {
        for scope in [
            LocalCapabilityScope::path(PathBuf::new()).unwrap_err(),
            LocalCapabilityScope::host("").unwrap_err(),
            LocalCapabilityScope::host("   ").unwrap_err(),
            LocalCapabilityScope::secret("").unwrap_err(),
            LocalCapabilityScope::secret("\t").unwrap_err(),
        ] {
            assert_eq!(scope, LocalCapabilityGrantError::EmptyScope);
        }
    }

    #[test]
    fn rejects_malformed_scopes() {
        assert!(matches!(
            LocalCapabilityScope::path("relative/path").unwrap_err(),
            LocalCapabilityGrantError::InvalidScope { .. }
        ));
        assert!(matches!(
            LocalCapabilityScope::host("db .internal").unwrap_err(),
            LocalCapabilityGrantError::InvalidScope { .. }
        ));
        assert!(matches!(
            LocalCapabilityScope::host(" db.internal").unwrap_err(),
            LocalCapabilityGrantError::InvalidScope { .. }
        ));
        assert!(matches!(
            LocalCapabilityScope::secret(" key").unwrap_err(),
            LocalCapabilityGrantError::InvalidScope { .. }
        ));
        assert!(matches!(
            LocalCapabilityScope::secret("key ").unwrap_err(),
            LocalCapabilityGrantError::InvalidScope { .. }
        ));
    }

    #[test]
    fn path_scope_covers_exact_and_subpath_values() {
        let scope = path_scope("/home/bob");
        for covered in [
            "/home/bob",
            "/home/bob/",
            "/home/bob/notes.txt",
            "/home/bob/x/y",
            "/home/bob/x/../y",
            "/home/bob/./notes.txt",
        ] {
            assert!(scope.satisfies(covered), "covered: {covered}");
        }
    }

    #[test]
    fn path_scope_rejects_siblings_parents_escapes_and_relative_values() {
        let scope = path_scope("/home/bob");
        for denied in [
            "/home",
            "/home/bobette",
            "/home/bob-x",
            "/home/bob/../etc",
            "/home/bob/../../etc",
            "/home/bob/../bobette",
            "/etc",
            "/",
            "home/bob",
            "home/bob/x",
            "",
            "/home/bob/..",
        ] {
            assert!(!scope.satisfies(denied), "denied: {denied}");
        }
    }

    #[test]
    fn normalises_grant_scopes_lexically_at_construction() {
        let scope = path_scope("/home/bob/../tmp");
        let LocalCapabilityScope::Path(path) = &scope else {
            panic!("expected a path scope");
        };
        assert_eq!(path.as_path(), Path::new("/home/tmp"));
        assert!(scope.satisfies("/home/tmp/a"));
        assert!(!scope.satisfies("/home/bob/a"));
    }

    #[test]
    fn root_scope_covers_every_absolute_path() {
        let scope = path_scope("/");
        assert!(scope.satisfies("/etc/passwd"));
        assert!(scope.satisfies("/home/bob"));
        assert!(!scope.satisfies("etc/passwd"));
    }

    #[test]
    fn host_and_secret_scopes_require_exact_match() {
        let host = host_scope("db.internal");
        assert!(host.satisfies("db.internal"));
        for denied in ["db.internal:5432", "db.internal.", "DB.INTERNAL", "db"] {
            assert!(!host.satisfies(denied), "denied: {denied}");
        }

        let secret = secret_scope("payments-key");
        assert!(secret.satisfies("payments-key"));
        for denied in ["payments", " payments-key", "payments-key "] {
            assert!(!secret.satisfies(denied), "denied: {denied}");
        }
    }

    #[test]
    fn empty_set_denies_every_capability() {
        let set = LocalCapabilityGrantSet::new();
        assert!(set.is_empty());
        assert_eq!(set.len(), 0);
        assert!(!set.contains(LocalCapabilityName::StdFsRead));
        assert!(!set.satisfies(LocalCapabilityName::StdFsRead, "/home/bob"));
        assert!(!set.satisfies(LocalCapabilityName::StdNetConnect, "db.internal"));
    }

    #[test]
    fn set_contains_and_satisfies_across_grants() {
        let set = LocalCapabilityGrantSet::from_grants([
            grant("std.fs.read", path_scope("/home/bob")),
            grant("std.fs.read", path_scope("/tmp")),
            grant("std.net.connect", host_scope("db.internal")),
            grant("std.secret.use", secret_scope("payments-key")),
        ])
        .unwrap();

        assert!(set.contains(LocalCapabilityName::StdFsRead));
        assert!(set.contains(LocalCapabilityName::StdNetConnect));
        assert!(set.contains(LocalCapabilityName::StdSecretUse));
        assert!(!set.contains(LocalCapabilityName::StdFsWrite));

        assert!(set.satisfies(LocalCapabilityName::StdFsRead, "/home/bob/x"));
        assert!(set.satisfies(LocalCapabilityName::StdFsRead, "/tmp/y"));
        assert!(!set.satisfies(LocalCapabilityName::StdFsRead, "/etc"));
        assert!(set.satisfies(LocalCapabilityName::StdNetConnect, "db.internal"));
        assert!(!set.satisfies(LocalCapabilityName::StdNetConnect, "other.internal"));
        assert!(set.satisfies(LocalCapabilityName::StdSecretUse, "payments-key"));
        assert!(!set.satisfies(LocalCapabilityName::StdSecretUse, "other-key"));
        assert!(!set.satisfies(LocalCapabilityName::StdFsWrite, "/home/bob"));
    }

    #[test]
    fn set_preserves_first_occurrence_order() {
        let set = LocalCapabilityGrantSet::from_grants([
            grant("std.secret.use", secret_scope("a")),
            grant("std.fs.read", path_scope("/home/bob")),
            grant("std.net.connect", host_scope("db.internal")),
        ])
        .unwrap();

        let names: Vec<LocalCapabilityName> =
            set.as_slice().iter().map(|grant| grant.name()).collect();
        assert_eq!(
            names,
            [
                LocalCapabilityName::StdSecretUse,
                LocalCapabilityName::StdFsRead,
                LocalCapabilityName::StdNetConnect,
            ]
        );
    }

    #[test]
    fn set_rejects_exact_duplicate_grants_but_allows_distinct_scopes() {
        let error = LocalCapabilityGrantSet::from_grants([
            grant("std.fs.read", path_scope("/home/bob")),
            grant("std.fs.read", path_scope("/home/bob")),
        ])
        .unwrap_err();
        assert!(
            matches!(error, LocalCapabilityGrantError::DuplicateGrant { .. }),
            "{error}"
        );
        assert!(error.to_string().contains("duplicate"));

        // The same capability with a different scope is not a duplicate.
        let set = LocalCapabilityGrantSet::from_grants([
            grant("std.fs.read", path_scope("/home/bob")),
            grant("std.fs.read", path_scope("/tmp")),
        ])
        .unwrap();
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn satisfies_declaration_uses_literal_text_directly() {
        let set =
            LocalCapabilityGrantSet::from_grants([grant("std.fs.read", path_scope("/home/bob"))])
                .unwrap();

        let covered = LocalCapabilityDeclaration::new(
            LocalCapabilityName::StdFsRead,
            LocalCapabilityArgumentSource::Text("/home/bob/x".to_owned()),
        );
        assert!(set.satisfies_declaration(&covered, |_| unreachable!()));

        let uncovered = LocalCapabilityDeclaration::new(
            LocalCapabilityName::StdFsRead,
            LocalCapabilityArgumentSource::Text("/etc/passwd".to_owned()),
        );
        assert!(!set.satisfies_declaration(&uncovered, |_| unreachable!()));
    }

    #[test]
    fn satisfies_declaration_resolves_parameter_references_at_the_gate() {
        let set =
            LocalCapabilityGrantSet::from_grants([grant("std.fs.read", path_scope("/home/bob"))])
                .unwrap();

        let declaration = LocalCapabilityDeclaration::new(
            LocalCapabilityName::StdFsRead,
            LocalCapabilityArgumentSource::Parameter("p_file".to_owned()),
        );

        // The gate resolves the parameter; a covered value is satisfied.
        let bindings = [("p_file".to_owned(), "/home/bob/notes.txt".to_owned())];
        let resolve = |parameter: &str| {
            bindings
                .iter()
                .find(|(name, _)| name == parameter)
                .map(|(_, value)| value.clone())
        };
        assert!(set.satisfies_declaration(&declaration, resolve));

        // A resolved value outside the grant scope is denied.
        let resolve_outside = |parameter: &str| {
            if parameter == "p_file" {
                Some("/etc/passwd".to_owned())
            } else {
                None
            }
        };
        assert!(!set.satisfies_declaration(&declaration, resolve_outside));

        // An unresolved parameter fails closed.
        assert!(!set.satisfies_declaration(&declaration, |_| None));

        // A declared capability with no grant is denied.
        let missing = LocalCapabilityDeclaration::new(
            LocalCapabilityName::StdFsWrite,
            LocalCapabilityArgumentSource::Parameter("p_file".to_owned()),
        );
        assert!(!set.satisfies_declaration(&missing, resolve_outside));
    }

    #[test]
    fn display_forms_are_exact_for_names_grants_and_errors() {
        assert_eq!(LocalCapabilityName::StdFsRead.to_string(), "std.fs.read");
        assert_eq!(
            grant("std.fs.read", path_scope("/home/bob")).to_string(),
            "std.fs.read(/home/bob)"
        );
        assert_eq!(
            grant("std.net.connect", host_scope("db.internal")).to_string(),
            "std.net.connect(db.internal)"
        );
        assert_eq!(
            grant("std.secret.use", secret_scope("payments-key")).to_string(),
            "std.secret.use(payments-key)"
        );
        assert_eq!(
            LocalCapabilityGrantError::UnknownCapability {
                name: "std.fs.call".to_owned()
            }
            .to_string(),
            "unknown local capability `std.fs.call`"
        );
        assert_eq!(
            LocalCapabilityGrantError::EmptyScope.to_string(),
            "local capability scope must not be empty"
        );
    }
}
