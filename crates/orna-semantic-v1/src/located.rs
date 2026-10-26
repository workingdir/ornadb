//! Module and item origins for checker diagnostics.
//!
//! Checker diagnostics do not carry source spans yet. The real fix is U01-03:
//! every checker diagnostic is built with the span of the node that raised it.
//! Until then, [`analyze_with_catalogue_located`] reports where analysis was
//! when it emitted each diagnostic, using only data the analysis already
//! holds: the logical path of the module being checked and the span of the
//! top-level item being checked. The field names follow the planned shared
//! `SourceDiagnostic` (logical path and primary span) so the shared diagnostic
//! slice can absorb this type; the code, severity and message stay on the
//! existing [`Diagnostic`].

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ops::Range;

use orna_foundation_v1::Diagnostic;
use orna_syntax_v1::SyntaxSpan;

use crate::{Analysis, Catalogue, ModuleInput, Namespace};

/// Where analysis was when it emitted one diagnostic.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DiagnosticOrigin {
    /// Logical path of the module being checked, or `None` for diagnostics
    /// that belong to no single module.
    pub logical_path: Option<String>,
    /// Byte range, in that module, of the top-level item being checked.
    /// `None` when the diagnostic concerns the module as a whole, such as its
    /// path, namespace or imports.
    pub primary: Option<Range<usize>>,
}

/// An analysis with one origin per diagnostic.
#[derive(Clone, Debug)]
pub struct LocatedAnalysis {
    pub analysis: Analysis,
    /// `origins[i]` belongs to `analysis.diagnostics[i]`.
    pub origins: Vec<DiagnosticOrigin>,
}

/// Runs [`crate::analyze_with_catalogue`] and records the origin of every
/// diagnostic. The analysis, including diagnostic order, is identical.
pub fn analyze_with_catalogue_located(
    inputs: &[ModuleInput],
    catalogue: &Catalogue,
) -> LocatedAnalysis {
    let mut recorder = OriginRecorder {
        enabled: true,
        ..OriginRecorder::disabled()
    };
    let (analysis, _) = crate::analyze_recording_origins(
        inputs,
        catalogue,
        false,
        crate::SystemProviders::DeclaredSurface,
        &mut recorder,
    );
    let origins = recorder.align(&analysis.diagnostics);
    LocatedAnalysis { analysis, origins }
}

/// Attributes diagnostics to the module or item being checked when they were
/// pushed. A disabled recorder does no work.
pub(crate) struct OriginRecorder {
    enabled: bool,
    current: DiagnosticOrigin,
    /// Number of diagnostics already attributed.
    attributed: usize,
    /// Emitted diagnostics with their origins, in emission order.
    emitted: Vec<(Diagnostic, DiagnosticOrigin)>,
    paths: BTreeMap<Namespace, String>,
}

impl OriginRecorder {
    pub(crate) fn disabled() -> Self {
        Self {
            enabled: false,
            current: DiagnosticOrigin::default(),
            attributed: 0,
            emitted: Vec::new(),
            paths: BTreeMap::new(),
        }
    }

    pub(crate) fn enter_module(&mut self, diagnostics: &[Diagnostic], logical_path: &str) {
        self.advance(
            diagnostics,
            DiagnosticOrigin {
                logical_path: Some(logical_path.to_owned()),
                primary: None,
            },
        );
    }

    pub(crate) fn bind_namespace(&mut self, namespace: &Namespace, logical_path: &str) {
        if self.enabled {
            self.paths
                .insert(namespace.clone(), logical_path.to_owned());
        }
    }

    pub(crate) fn enter_namespace(&mut self, diagnostics: &[Diagnostic], namespace: &Namespace) {
        let logical_path = self.paths.get(namespace).cloned();
        self.advance(
            diagnostics,
            DiagnosticOrigin {
                logical_path,
                primary: None,
            },
        );
    }

    pub(crate) fn enter_item(
        &mut self,
        diagnostics: &[Diagnostic],
        namespace: &Namespace,
        span: &SyntaxSpan,
    ) {
        let logical_path = self.paths.get(namespace).cloned();
        self.advance(
            diagnostics,
            DiagnosticOrigin {
                logical_path,
                primary: Some(span.start..span.end),
            },
        );
    }

    pub(crate) fn finish(&mut self, diagnostics: &[Diagnostic]) {
        self.advance(diagnostics, DiagnosticOrigin::default());
    }

    fn advance(&mut self, diagnostics: &[Diagnostic], next: DiagnosticOrigin) {
        if !self.enabled {
            return;
        }
        for diagnostic in &diagnostics[self.attributed..] {
            self.emitted
                .push((diagnostic.clone(), self.current.clone()));
        }
        self.attributed = diagnostics.len();
        self.current = next;
    }

    /// Pairs the final diagnostic order with the recorded origins. Analysis
    /// reorders diagnostics with a stable sort, so diagnostics that compare
    /// equal keep their emission order; matching each final diagnostic to the
    /// earliest unused equal emission therefore restores its origin.
    fn align(self, diagnostics: &[Diagnostic]) -> Vec<DiagnosticOrigin> {
        let mut pending: HashMap<(&str, &str), VecDeque<usize>> = HashMap::new();
        for (index, (diagnostic, _)) in self.emitted.iter().enumerate() {
            pending
                .entry((diagnostic.code(), diagnostic.message()))
                .or_default()
                .push_back(index);
        }
        diagnostics
            .iter()
            .map(|diagnostic| {
                let queue = pending.get_mut(&(diagnostic.code(), diagnostic.message()));
                let index = queue.and_then(|queue| {
                    let position = queue
                        .iter()
                        .position(|index| self.emitted[*index].0 == *diagnostic)?;
                    queue.remove(position)
                });
                index.map_or_else(DiagnosticOrigin::default, |index| {
                    self.emitted[index].1.clone()
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_name_the_module_and_item_that_raised_each_diagnostic() {
        let main = "pub fn fine(): Int = 1;\npub fn uses(): Int = missing_name + 1;\n";
        let util = "pub fn wrong(): Int = \"text\";\n";
        let inputs = [
            ModuleInput::new("main.orna", main),
            ModuleInput::new("util.orna", util),
        ];
        let catalogue = Catalogue::authoritative_core();
        let located = analyze_with_catalogue_located(&inputs, &catalogue);
        let plain = crate::analyze_with_catalogue(&inputs, &catalogue);

        assert_eq!(located.analysis.diagnostics, plain.diagnostics);
        assert_eq!(located.origins.len(), located.analysis.diagnostics.len());
        let origin_of = |code: &str| {
            let index = located
                .analysis
                .diagnostics
                .iter()
                .position(|diagnostic| diagnostic.code() == code)
                .unwrap_or_else(|| panic!("{code} diagnostic"));
            located.origins[index].clone()
        };
        let unresolved = origin_of(crate::DIAG_UNRESOLVED);
        assert_eq!(unresolved.logical_path.as_deref(), Some("main.orna"));
        let item = unresolved.primary.expect("item span");
        assert!(
            item.contains(&main.find("missing_name").unwrap()),
            "{item:?}"
        );
        assert!(!item.contains(&main.find("fine").unwrap()), "{item:?}");

        let mismatch = origin_of(crate::DIAG_TYPE);
        assert_eq!(mismatch.logical_path.as_deref(), Some("util.orna"));
        let item = mismatch.primary.expect("item span");
        assert!(item.contains(&util.find("\"text\"").unwrap()), "{item:?}");
    }

    #[test]
    fn module_level_diagnostics_have_no_item_span() {
        let inputs = [ModuleInput::new("std/main.orna", "pub fn one(): Int = 1;")];
        let located = analyze_with_catalogue_located(&inputs, &Catalogue::authoritative_core());
        assert_eq!(located.analysis.diagnostics.len(), 1);
        assert_eq!(
            located.origins,
            [DiagnosticOrigin {
                logical_path: Some("std/main.orna".to_owned()),
                primary: None,
            }]
        );
    }
}
