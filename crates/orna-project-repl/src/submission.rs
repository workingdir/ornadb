//! REPL submission with a read-only table-scan path.
//!
//! An input is admitted by its static effect summary. Table reads are the only
//! effect the read path accepts: the caller serves them through an
//! [`EffectHandler`] over a committed snapshot, so the input cannot write. Every
//! other effectful input is refused before evaluation.

use orna_check::{EffectKind, EffectSummary};
use orna_evaluator::{EffectHandler, RelationDisplay};
use orna_value::CanonicalValue;

use super::{AdmittedReplSession, ReplError, ReplOutcome, StagedReplActivation, effect_rejection};

impl AdmittedReplSession {
    /// Executes one input and returns a relation as a bounded row window. Table
    /// reads are served by `effects`. The session advances only on success, as
    /// [`Self::submit_outcome`] does.
    pub fn submit_read_outcome(
        &mut self,
        source: &str,
        effects: &mut dyn EffectHandler,
    ) -> Result<ReplOutcome, ReplError> {
        self.submit_outcome_with(source, RelationDisplay::Window, Some(effects))
    }

    /// As [`Self::submit`], with the table reads of the input served by
    /// `effects`, and so the system relations it reads. An input with any other
    /// effect is refused before evaluation, and an input with no effect
    /// evaluates as [`Self::submit`] does. A relation result is an error, as it
    /// is for [`Self::submit`].
    pub fn submit_with_effects(
        &mut self,
        source: &str,
        effects: &mut dyn EffectHandler,
    ) -> Result<Option<CanonicalValue>, ReplError> {
        self.submit_outcome_with(source, RelationDisplay::Reject, Some(effects))?
            .value
            .map(|value| value.into_canonical().map_err(ReplError::runtime))
            .transpose()
    }

    /// Admits one input and executes it. Table reads are served by `effects`
    /// when given. Any other effect is refused before evaluation.
    pub(super) fn submit_outcome_with(
        &mut self,
        source: &str,
        display: RelationDisplay,
        effects: Option<&mut dyn EffectHandler>,
    ) -> Result<ReplOutcome, ReplError> {
        let input = match self.parse(source) {
            Ok(input) => input,
            Err(error) => return Err(self.publish_failure(error)),
        };
        let admission = match self.stage_input(&input, source) {
            Ok(admission) => admission,
            Err(error) => return Err(self.publish_failure(error)),
        };
        let reads_served = effects.is_some() && reads_tables_only(&admission.effects);
        if admission.effects.is_effectful() && !reads_served {
            return Err(self.publish_failure(effect_rejection(&admission.effects)));
        }
        let ty = admission.ty.clone();
        let mut runtime = self.runtime.clone();
        let value = match runtime
            .submit_admitted_with(&input, None, effects, display)
            .map_err(ReplError::runtime)
        {
            Ok(value) => value,
            Err(error) => return Err(self.publish_failure(error)),
        };
        let mut semantic = self.semantic.clone();
        if semantic.commit(admission).is_err() {
            return Err(self.publish_failure(ReplError::fixed("ORNA-REPL-COMMIT")));
        }
        runtime.set_last_status(
            CanonicalValue::new(orna_value::OvbRaw::Null).expect("null is canonical"),
        );
        self.runtime = runtime;
        self.semantic = semantic;
        Ok(ReplOutcome { value, ty })
    }
}

impl StagedReplActivation {
    /// Whether every effect is a table read, so a committed read snapshot can
    /// serve the activation without a writer.
    #[must_use]
    pub fn is_table_read_only(&self) -> bool {
        reads_tables_only(self.effects())
    }
}

impl ReplError {
    /// The committed read scope for a pure REPL input could not be built.
    #[must_use]
    pub fn read_scope_unavailable() -> Self {
        Self::fixed("ORNA-REPL-EFFECT")
    }
}

/// Whether `effects` holds at least one effect, every one a table read, and no
/// opaque effect.
fn reads_tables_only(effects: &EffectSummary) -> bool {
    !effects.opaque
        && !effects.effects.is_empty()
        && effects
            .effects
            .iter()
            .all(|kind| *kind == EffectKind::TableRead)
}
