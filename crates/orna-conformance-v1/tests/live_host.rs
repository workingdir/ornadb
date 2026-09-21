//! Reachability checks for the public live action-authority API.

use orna_foundation_v1::CanonicalValue;
use orna_live_v1::{
    ActionAuthority, ActionAuthorityRegistry, ActionBinding, ActionFuture, ActionHandler, Error,
    LiveApplicationWorkLease,
};
use orna_runtime_v1::RuntimeActivationContext;

struct RejectingActionHandler;

impl ActionHandler for RejectingActionHandler {
    fn accepts(&self, _: &CanonicalValue) -> bool {
        false
    }

    fn activate<'a>(
        &'a self,
        _: ActionBinding,
        _: [u8; 16],
        _: [u8; 32],
        _: &'a CanonicalValue,
        _: &'a RuntimeActivationContext,
        _: &'a mut LiveApplicationWorkLease,
    ) -> ActionFuture<'a> {
        Box::pin(async { Err(Error::ApplicationRejected) })
    }
}

fn assert_action_authority<T: ActionAuthority>(_: &T) {}

#[test]
fn public_action_registry_reaches_registration_boundaries() {
    let binding = ActionBinding {
        session: [1; 16],
        watch: [2; 16],
        page_revision: 7,
        action: [3; 16],
    };
    let mut registry = ActionAuthorityRegistry::new();
    assert_action_authority(&registry);

    assert!(registry.register(binding, RejectingActionHandler).is_ok());
    assert_eq!(
        registry.register(binding, RejectingActionHandler),
        Err(orna_live_v1::ActionRegistrationError::DuplicateBinding)
    );
    assert_eq!(
        registry.register(
            ActionBinding {
                action: [0; 16],
                ..binding
            },
            RejectingActionHandler,
        ),
        Err(orna_live_v1::ActionRegistrationError::ZeroHandle)
    );
}