use fuser::Errno;

use super::{DraftState, noncandidate_ack};

#[test]
fn clean_handle_without_candidate_acknowledges() {
    let mut draft_state = DraftState::default();
    assert_eq!(noncandidate_ack(&mut draft_state, 7, "fsync"), Ok(()));
}

#[test]
fn dirty_handle_without_candidate_fails_closed_with_eio() {
    let mut draft_state = DraftState::default();
    draft_state.set_unaccepted_write(7);
    assert_eq!(
        noncandidate_ack(&mut draft_state, 7, "fsync"),
        Err(Errno::EIO)
    );
}
