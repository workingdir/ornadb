//! Save state machine for a writable VFS-1 managed file (EDIT-1).
//!
//! `write`, `truncate` and sparse or out-of-order byte writes only advance the
//! private draft. Acceptance happens at `fsync`/`fdatasync`: the caller takes a
//! [`SyncTicket`] for one draft sequence, validates that exact candidate against
//! the shared authority, and reports the verdict with [`SaveMachine::complete`].
//! `release` never accepts state; it only reports retained unsynced drafts.

use std::collections::BTreeMap;

/// Current save state of one managed file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SaveState {
    /// No unaccepted draft is pending.
    Clean,
    /// A private draft exists that has not been submitted for validation.
    Dirty,
    /// One draft sequence is being validated by the shared authority.
    Validating,
    /// The last validated draft was rejected. The draft is retained.
    Rejected,
    /// The last validated draft conflicted with the current baseline.
    Conflicted,
}

/// Authority verdict for one validated draft sequence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SaveVerdict {
    Accepted,
    Rejected { diagnostic: String },
    Conflicted,
}

/// The recorded outcome of one draft sequence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SaveOutcome {
    Accepted,
    Rejected { diagnostic: String },
    Conflicted,
}

/// Result of `fsync`/`fdatasync`: either a new validation to run, or the
/// outcome already recorded for the same draft sequence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SyncRequest {
    Validate(SyncTicket),
    Recorded { seq: u64, outcome: SaveOutcome },
}

/// Permission to validate exactly one draft sequence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyncTicket {
    seq: u64,
}

impl SyncTicket {
    pub fn seq(self) -> u64 {
        self.seq
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SaveError {
    /// A sync for the current draft is already being validated.
    ValidationInProgress,
    /// There is no unaccepted draft to synchronize.
    NothingToSync,
    /// A verdict was reported while no validation is in flight.
    NotValidating { found: u64 },
    /// A verdict was reported for a sequence other than the one in flight.
    WrongSequence { expected: u64, found: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validate(machine: &mut SaveMachine) -> SyncTicket {
        match machine.sync().unwrap() {
            SyncRequest::Validate(ticket) => ticket,
            other => panic!("expected a validation ticket, got {other:?}"),
        }
    }

    #[test]
    fn accepted_sync_moves_accepted_sequence_and_returns_clean() {
        let mut machine = SaveMachine::new();
        assert_eq!(machine.write().unwrap(), 1);
        assert_eq!(machine.state(), SaveState::Dirty);
        let ticket = validate(&mut machine);
        assert_eq!(ticket.seq(), 1);
        assert_eq!(machine.state(), SaveState::Validating);
        assert_eq!(
            machine.complete(ticket, SaveVerdict::Accepted).unwrap(),
            SaveOutcome::Accepted
        );
        assert_eq!(machine.state(), SaveState::Clean);
        assert_eq!(machine.accepted_seq(), 1);
    }

    #[test]
    fn rejected_draft_is_retained_and_leaves_accepted_state_unchanged() {
        let mut machine = SaveMachine::new();
        machine.write().unwrap();
        let ticket = validate(&mut machine);
        machine
            .complete(
                ticket,
                SaveVerdict::Rejected {
                    diagnostic: "schema".to_owned(),
                },
            )
            .unwrap();
        assert_eq!(machine.state(), SaveState::Rejected);
        assert_eq!(machine.accepted_seq(), 0);
        assert_eq!(machine.release(), None, "rejected drafts are not dirty");
    }

    #[test]
    fn repeated_sync_for_same_sequence_returns_recorded_outcome() {
        let mut machine = SaveMachine::new();
        machine.write().unwrap();
        let ticket = validate(&mut machine);
        machine.complete(ticket, SaveVerdict::Accepted).unwrap();
        assert_eq!(
            machine.sync().unwrap(),
            SyncRequest::Recorded {
                seq: 1,
                outcome: SaveOutcome::Accepted,
            }
        );
    }

    #[test]
    fn later_write_starts_new_candidate_without_duplicating_accepted_mutation() {
        let mut machine = SaveMachine::new();
        machine.write().unwrap();
        let first = validate(&mut machine);
        machine.complete(first, SaveVerdict::Accepted).unwrap();
        assert_eq!(machine.write().unwrap(), 2);
        assert_eq!(machine.state(), SaveState::Dirty);
        assert_eq!(machine.accepted_seq(), 1);
        let second = validate(&mut machine);
        assert_eq!(second.seq(), 2);
    }

    #[test]
    fn release_never_accepts_dirty_state() {
        let mut machine = SaveMachine::new();
        machine.write().unwrap();
        assert_eq!(machine.release(), Some(1));
        assert_eq!(machine.accepted_seq(), 0);
        assert_eq!(machine.state(), SaveState::Dirty);
    }

    #[test]
    fn writes_and_second_sync_are_rejected_while_validating() {
        let mut machine = SaveMachine::new();
        machine.write().unwrap();
        let _ticket = validate(&mut machine);
        assert_eq!(machine.write(), Err(SaveError::ValidationInProgress));
        assert_eq!(machine.sync(), Err(SaveError::ValidationInProgress));
    }

    #[test]
    fn verdict_for_a_different_sequence_is_refused() {
        let mut machine = SaveMachine::new();
        machine.write().unwrap();
        let ticket = validate(&mut machine);
        let stale = SyncTicket { seq: ticket.seq() + 1 };
        assert_eq!(
            machine.complete(stale, SaveVerdict::Accepted),
            Err(SaveError::WrongSequence { expected: 1, found: 2 })
        );
        assert_eq!(machine.state(), SaveState::Validating);
    }

    #[test]
    fn sync_with_no_draft_is_nothing_to_sync() {
        let mut machine = SaveMachine::new();
        assert_eq!(machine.sync(), Err(SaveError::NothingToSync));
    }
}

/// Save machine for one managed file.
#[derive(Debug)]
pub struct SaveMachine {
    state: SaveState,
    accepted_seq: u64,
    draft_seq: u64,
    outcomes: BTreeMap<u64, SaveOutcome>,
    validating_seq: Option<u64>,
}

impl Default for SaveMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl SaveMachine {
    pub fn new() -> Self {
        Self {
            state: SaveState::Clean,
            accepted_seq: 0,
            draft_seq: 0,
            outcomes: BTreeMap::new(),
            validating_seq: None,
        }
    }

    pub fn state(&self) -> SaveState {
        self.state
    }

    /// Sequence of the last draft the authority accepted (0 before any).
    pub fn accepted_seq(&self) -> u64 {
        self.accepted_seq
    }

    /// Sequence of the current private draft (0 before any write).
    pub fn draft_seq(&self) -> u64 {
        self.draft_seq
    }

    /// Records a private write (`write`, `truncate`, sparse or out-of-order
    /// bytes). Writes never touch accepted state. A write after a rejection or
    /// conflict starts a new candidate based on the accepted state.
    pub fn write(&mut self) -> Result<u64, SaveError> {
        if self.state == SaveState::Validating {
            return Err(SaveError::ValidationInProgress);
        }
        self.draft_seq += 1;
        self.state = SaveState::Dirty;
        Ok(self.draft_seq)
    }

    /// Starts validation for the current draft on `fsync`/`fdatasync`.
    ///
    /// A repeated callback for a sequence that already has a recorded outcome
    /// returns that outcome instead of validating again.
    pub fn sync(&mut self) -> Result<SyncRequest, SaveError> {
        match self.state {
            SaveState::Validating => Err(SaveError::ValidationInProgress),
            SaveState::Dirty => {
                self.state = SaveState::Validating;
                self.validating_seq = Some(self.draft_seq);
                Ok(SyncRequest::Validate(SyncTicket {
                    seq: self.draft_seq,
                }))
            }
            SaveState::Clean | SaveState::Rejected | SaveState::Conflicted => {
                match self.outcomes.get(&self.draft_seq) {
                    Some(outcome) => Ok(SyncRequest::Recorded {
                        seq: self.draft_seq,
                        outcome: outcome.clone(),
                    }),
                    None => Err(SaveError::NothingToSync),
                }
            }
        }
    }

    /// Records the authority verdict for the sequence in `ticket`.
    ///
    /// Acceptance moves the accepted sequence forward and returns to `Clean`.
    /// Rejection and conflict keep the draft and leave accepted state
    /// unchanged.
    pub fn complete(&mut self, ticket: SyncTicket, verdict: SaveVerdict) -> Result<SaveOutcome, SaveError> {
        let Some(expected) = self.validating_seq.filter(|_| self.state == SaveState::Validating)
        else {
            return Err(SaveError::NotValidating { found: ticket.seq });
        };
        if expected != ticket.seq {
            return Err(SaveError::WrongSequence {
                expected,
                found: ticket.seq,
            });
        }
        self.validating_seq = None;
        let outcome = match verdict {
            SaveVerdict::Accepted => {
                self.accepted_seq = ticket.seq;
                self.state = SaveState::Clean;
                SaveOutcome::Accepted
            }
            SaveVerdict::Rejected { diagnostic } => {
                self.state = SaveState::Rejected;
                SaveOutcome::Rejected { diagnostic }
            }
            SaveVerdict::Conflicted => {
                self.state = SaveState::Conflicted;
                SaveOutcome::Conflicted
            }
        };
        self.outcomes.insert(ticket.seq, outcome.clone());
        Ok(outcome)
    }

    /// Handle release. Cleanup only: it never accepts a draft. Returns the
    /// unsynced draft sequence that remains retained, if any.
    pub fn release(&self) -> Option<u64> {
        match self.state {
            SaveState::Dirty => Some(self.draft_seq),
            _ => None,
        }
    }
}
