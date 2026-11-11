//! The link plan: the binding of every native declaration, and the verified
//! library files the plan uses (docs/design/native-libraries.md, section 10,
//! steps 1 to 3). Nothing here loads a library or calls one. The plan is built
//! before evaluation, and a non-empty error list stops the run.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use orna_base::hex;
use orna_hir::{
    DefId, DefKind, ENGINE_NATIVE_ABI, FnBody, NativeAbi, NativeBinding, NativeKey, Program,
};

use crate::error::{DeclSite, LinkError, LinkErrorCode};
use crate::file::{resolve_library_file, sha256_file};
use crate::manifest::{NativeLibrary, ProjectManifest};

/// The library ABI this engine loads: the number `orna_library_abi()` must return.
pub const LIBRARY_NATIVE_ABI: u32 = 1;

/// The profile a program links for (docs/design/native-libraries.md, section 7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkProfile {
    /// The native host: registry natives and shared libraries.
    Native,
    /// The browser Wasm host: registry natives only.
    BrowserWasm,
}

/// The keys the engine's native registry implements. The host supplies it; this
/// crate does not own the implementations (docs/design/hir.md, section 4.3).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EngineRegistry {
    natives: BTreeSet<NativeKey>,
}

impl EngineRegistry {
    /// A registry holding `natives`.
    #[must_use]
    pub fn new(natives: impl IntoIterator<Item = NativeKey>) -> Self {
        Self {
            natives: natives.into_iter().collect(),
        }
    }

    /// Whether the host implements `key`.
    #[must_use]
    pub fn contains(&self, key: &NativeKey) -> bool {
        self.natives.contains(key)
    }
}

/// How one native declaration binds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Binding {
    /// The engine registry implements the declaration.
    Engine {
        /// The declaration's key in the registry.
        key: NativeKey,
    },
    /// The named library exports the declaration's symbol.
    Library {
        /// The library's manifest name.
        library: String,
        /// The C symbol the library must export.
        symbol: String,
    },
}

/// A library the plan uses, after its file has passed every check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedLibrary {
    /// The file to load, under the project root.
    pub file: PathBuf,
    /// The SHA-256 the file matched.
    pub sha256: [u8; 32],
}

/// A successful link plan.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkPlan {
    /// The binding of every native declaration, by definition.
    pub bindings: BTreeMap<DefId, Binding>,
    /// The libraries in use, by manifest name.
    pub libraries: BTreeMap<String, PlannedLibrary>,
}

/// One native declaration, with the identity the errors name.
struct Declaration<'a> {
    id: DefId,
    key: &'a NativeKey,
    binding: &'a NativeBinding,
    site: DeclSite,
}

/// Builds the link plan for `program`.
///
/// `root` is the project root the manifest paths are relative to, and `target`
/// is the target triple whose file entries apply.
///
/// # Errors
///
/// Returns every failure found, each naming its declaration and, for library
/// failures, the library. A non-empty list means nothing may be loaded or run.
pub fn plan(
    program: &Program,
    manifest: &ProjectManifest,
    root: &Path,
    target: &str,
    profile: LinkProfile,
    registry: &EngineRegistry,
) -> Result<LinkPlan, Vec<LinkError>> {
    let declarations = declarations(program);
    let mut errors = Vec::new();
    let mut plan = LinkPlan::default();
    let mut used: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, declaration) in declarations.iter().enumerate() {
        match declaration.binding {
            NativeBinding::Engine { abi } => {
                if let Some(binding) =
                    bind_engine(declaration, *abi, profile, registry, &mut errors)
                {
                    plan.bindings.insert(declaration.id, binding);
                }
            }
            NativeBinding::Library { .. } => {
                let module = declaration.key.module.to_string();
                let serving: Vec<&str> = manifest
                    .libraries
                    .iter()
                    .filter(|library| library.modules.contains(&module))
                    .map(|library| library.name.as_str())
                    .collect();
                match serving.as_slice() {
                    [] => errors.push(
                        LinkError::new(
                            LinkErrorCode::Unbound,
                            format!("no registry entry and no library serves module `{module}`"),
                        )
                        .with_declarations(vec![declaration.site.clone()]),
                    ),
                    [library] => used.entry(*library).or_default().push(index),
                    many => errors.push(
                        LinkError::new(
                            LinkErrorCode::Duplicate,
                            format!(
                                "libraries `{}` all list module `{module}`",
                                many.join("`, `")
                            ),
                        )
                        .with_declarations(vec![declaration.site.clone()]),
                    ),
                }
            }
        }
    }
    for library in &manifest.libraries {
        let Some(indices) = used.get(library.name.as_str()) else {
            continue;
        };
        let served: Vec<DeclSite> = indices
            .iter()
            .map(|&index| declarations[index].site.clone())
            .collect();
        match check_library(library, root, target, profile) {
            Ok(planned) => {
                for &index in indices {
                    let declaration = &declarations[index];
                    if let NativeBinding::Library { symbol } = declaration.binding {
                        plan.bindings.insert(
                            declaration.id,
                            Binding::Library {
                                library: library.name.clone(),
                                symbol: symbol.clone(),
                            },
                        );
                    }
                }
                plan.libraries.insert(library.name.clone(), planned);
            }
            Err(error) => errors.push(error.with_declarations(served)),
        }
    }
    if errors.is_empty() {
        Ok(plan)
    } else {
        Err(errors)
    }
}

fn declarations(program: &Program) -> Vec<Declaration<'_>> {
    program
        .defs
        .iter_enumerated()
        .filter_map(|(id, def)| {
            let DefKind::Fn(function) = &def.kind else {
                return None;
            };
            let FnBody::Native(decl) = &function.body else {
                return None;
            };
            Some(Declaration {
                id,
                key: &decl.key,
                binding: &decl.binding,
                site: DeclSite::new(
                    decl.key.module.to_string(),
                    decl.key.name.to_string(),
                    &def.span,
                ),
            })
        })
        .collect()
}

/// Binds an engine declaration to the registry (step 3 for its ABI, step 1 for
/// its lookup). Returns `None` after recording the failure.
fn bind_engine(
    declaration: &Declaration<'_>,
    abi: NativeAbi,
    profile: LinkProfile,
    registry: &EngineRegistry,
    errors: &mut Vec<LinkError>,
) -> Option<Binding> {
    if abi != ENGINE_NATIVE_ABI {
        errors.push(
            LinkError::new(
                LinkErrorCode::Abi,
                format!(
                    "declaration is written against engine native ABI {}, the engine provides {}",
                    abi.number(),
                    ENGINE_NATIVE_ABI.number()
                ),
            )
            .with_declarations(vec![declaration.site.clone()]),
        );
        return None;
    }
    if registry.contains(declaration.key) {
        return Some(Binding::Engine {
            key: declaration.key.clone(),
        });
    }
    let error = match profile {
        LinkProfile::Native => LinkError::new(
            LinkErrorCode::Unbound,
            format!(
                "no engine registry entry implements `{}`",
                declaration.key.name
            ),
        ),
        LinkProfile::BrowserWasm => LinkError::new(
            LinkErrorCode::Profile,
            format!(
                "the browser profile has no registry entry for `{}`",
                declaration.key.name
            ),
        ),
    };
    errors.push(error.with_declarations(vec![declaration.site.clone()]));
    None
}

/// Checks one library against the profile, the target entry, the file and the
/// manifest digest (steps 2a to 2e). The first failure stops the library.
fn check_library(
    library: &NativeLibrary,
    root: &Path,
    target: &str,
    profile: LinkProfile,
) -> Result<PlannedLibrary, LinkError> {
    let named = |error: LinkError| error.with_library(&library.name);
    if profile == LinkProfile::BrowserWasm {
        return Err(named(LinkError::new(
            LinkErrorCode::Profile,
            "the browser profile cannot load a shared library",
        )));
    }
    let Some(entry) = library.targets.get(target) else {
        return Err(named(LinkError::new(
            LinkErrorCode::Library,
            format!("the manifest has no entry for target `{target}`"),
        )));
    };
    let path = entry.path.display();
    let file = resolve_library_file(root, &entry.path).map_err(|reason| {
        named(LinkError::new(
            LinkErrorCode::Library,
            format!("target `{target}` path `{path}` {reason}"),
        ))
    })?;
    let actual = sha256_file(&file).map_err(|error| {
        named(LinkError::new(
            LinkErrorCode::Library,
            format!("target `{target}` path `{path}` cannot be read ({error})"),
        ))
    })?;
    if actual != entry.sha256 {
        return Err(named(LinkError::new(
            LinkErrorCode::Digest,
            format!(
                "target `{target}` path `{path}`: expected sha256 {}, found {}",
                hex(&entry.sha256),
                hex(&actual)
            ),
        )));
    }
    if library.abi != LIBRARY_NATIVE_ABI {
        return Err(named(LinkError::new(
            LinkErrorCode::Abi,
            format!(
                "manifest declares library ABI {}, the engine loads ABI {LIBRARY_NATIVE_ABI}",
                library.abi
            ),
        )));
    }
    Ok(PlannedLibrary {
        file,
        sha256: entry.sha256,
    })
}
