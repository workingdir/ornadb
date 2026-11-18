//! Named declarations of a loaded project, with their doc comments and source
//! text. The REPL's `:doc` and `:source` read these records, and `:open` reads
//! the file and span a record was parsed from; they are taken from the same
//! parse that admits each module, so no module is parsed twice.

use std::ops::Range;
use std::path::{Path, PathBuf};

use orna_syntax_ast::{Declaration, Item};

/// Where one declaration was read from: the file its module came from, and the
/// byte span of the declaration within that file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclarationLocation {
    path: PathBuf,
    span: Range<usize>,
}

impl DeclarationLocation {
    /// The file the declaring module was read from.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The declaration's byte range within that file.
    #[must_use]
    pub fn span(&self) -> &Range<usize> {
        &self.span
    }
}

/// One named top-level declaration of an admitted module.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclarationRecord {
    /// Namespace components of the declaring module, in source order. The
    /// root `main.orna` contributes none.
    namespace: Vec<String>,
    /// The declared name, without its namespace.
    name: String,
    /// The `///` doc comment lines written directly above the declaration.
    docs: Vec<String>,
    /// The declaration's exact source text, from its first keyword through its
    /// last byte, as written in the declaring module.
    source: String,
    /// The file and byte span the declaration was read from, when its module
    /// came from a file of the working tree. A module read from an in-memory
    /// overlay, a committed snapshot, an attached database or the standard
    /// library has none.
    location: Option<DeclarationLocation>,
}

impl DeclarationRecord {
    /// The namespace components the declaration lives in.
    #[must_use]
    pub fn namespace(&self) -> &[String] {
        &self.namespace
    }

    /// The declared name without its namespace.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The dotted qualified name, such as `sensors.greenhouse.warm`.
    #[must_use]
    pub fn qualified_name(&self) -> String {
        let mut parts = self.namespace.clone();
        parts.push(self.name.clone());
        parts.join(".")
    }

    /// The doc comment lines, in source order.
    #[must_use]
    pub fn docs(&self) -> &[String] {
        &self.docs
    }

    /// The declaration's source text.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The file and byte span the declaration was read from, when its module
    /// came from a file of the working tree.
    #[must_use]
    pub fn location(&self) -> Option<&DeclarationLocation> {
        self.location.as_ref()
    }

    /// Re-homes this declaration under `prefix` namespace components, as an
    /// attached database is exposed under its alias. The result has no
    /// location: its module is not a file of this working tree.
    #[must_use]
    pub fn with_namespace_prefix(&self, prefix: &str) -> Self {
        let mut namespace = vec![prefix.to_owned()];
        namespace.extend(self.namespace.iter().cloned());
        Self {
            namespace,
            name: self.name.clone(),
            docs: self.docs.clone(),
            source: self.source.clone(),
            location: None,
        }
    }
}

/// The named declarations in `items`, which were parsed from `source` and
/// declared in `namespace`. `file` is the file `source` was read from, when it
/// was read from one. Declarations that bind no name (`use`, `let`,
/// assertions) have nothing to document and are omitted.
pub(crate) fn declaration_records(
    namespace: &[String],
    source: &str,
    items: &[Item],
    file: Option<&Path>,
) -> Vec<DeclarationRecord> {
    items
        .iter()
        .filter_map(|item| {
            let name = declared_name(&item.declaration)?;
            Some(DeclarationRecord {
                namespace: namespace.to_vec(),
                name: name.to_owned(),
                docs: item.docs.clone(),
                source: source[item.span.start..item.span.end].to_owned(),
                location: file.map(|path| DeclarationLocation {
                    path: path.to_owned(),
                    span: item.span.start..item.span.end,
                }),
            })
        })
        .collect()
}

fn declared_name(declaration: &Declaration) -> Option<&str> {
    match declaration {
        Declaration::Table { name, .. }
        | Declaration::NativeType { name, .. }
        | Declaration::NativeStatic { name, .. }
        | Declaration::Protocol { name, .. }
        | Declaration::Enum { name, .. }
        | Declaration::Dimension { name, .. }
        | Declaration::Unit { name, .. }
        | Declaration::Type { name, .. } => Some(name),
        Declaration::Function { signature, .. } | Declaration::Native { signature, .. } => {
            Some(&signature.name)
        }
        Declaration::Use { .. } | Declaration::Assertion { .. } | Declaration::Let { .. } => None,
    }
}
