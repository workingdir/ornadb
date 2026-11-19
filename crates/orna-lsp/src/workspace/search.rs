//! Reference search: every occurrence that refers to one target, and the
//! same-named occurrences that might.

use orna_hir::UseContext;

use super::{LocalBindingKind, Resolved, Search, Target, Workspace};

impl Workspace {
    /// Every occurrence that refers to `target`, and every same-named
    /// occurrence that might.
    pub(crate) fn search(&self, target: &Target) -> Search {
        let key = self.target_key(target);
        let member = match target {
            Target::Declared { module, index } => self.declaration(*module, *index).owner.is_some(),
            _ => false,
        };
        let parameter = match target {
            Target::Local { module, binding } => {
                self.binding(*module, *binding).kind == LocalBindingKind::Parameter
            }
            _ => false,
        };
        let mut search = Search::default();
        if let Target::Local { module, binding } = target {
            search
                .found
                .push((*module, self.binding(*module, *binding).selection.clone()));
        }
        if let Target::Declared { module, index } = target {
            search
                .found
                .push((*module, self.declaration(*module, *index).selection.clone()));
        }
        for (module, occurrences) in self.occurrences.iter().enumerate() {
            if self.modules[module]
                .path
                .segments()
                .first()
                .is_some_and(|name| name.as_str() == "std")
            {
                continue;
            }
            for occurrence in occurrences {
                if occurrence.key != key {
                    continue;
                }
                match &occurrence.resolved {
                    Resolved::Target(found) if found == target => {
                        search.found.push((module, occurrence.span.clone()));
                    }
                    Resolved::Target(_) => {}
                    Resolved::Ambiguous => search.uncertain.push((module, occurrence.span.clone())),
                    Resolved::Unknown => {
                        let possible = match occurrence.context {
                            UseContext::Field | UseContext::Label => member,
                            UseContext::Value => false,
                            _ => false,
                        } || (occurrence.context == UseContext::Label && parameter);
                        if possible {
                            search.uncertain.push((module, occurrence.span.clone()));
                        }
                    }
                }
            }
        }
        search
            .found
            .sort_by_key(|(module, span)| (*module, span.start, span.end));
        search
            .found
            .dedup_by(|right, left| left.0 == right.0 && left.1.start == right.1.start);
        search
    }
}
