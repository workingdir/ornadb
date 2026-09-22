use orna_protocol_v1::{Envelope, Message, PresentationContext};
use orna_serving_v1::{Credential, Error, Id, Limits, Origin, Patch, RetainedPin, Serving};

fn id(value: u8) -> Id {
    [value; 16]
}

fn pin(revision: u64) -> RetainedPin {
    RetainedPin {
        revision,
        fingerprint: [revision as u8; 32],
    }
}

fn subscribe(request: Id) -> Envelope {
    Envelope {
        request: Some(request),
        watch: None,
        message: Message::Subscribe {
            resource: id(9),
            presentation: PresentationContext {
                locale: "en-GB".into(),
                timezone: None,
                width: None,
                theme: "dark".into(),
                supported_kinds: vec![],
            },
        },
        extensions: Default::default(),
    }
}

fn admitted() -> Serving {
    let mut state = Serving::new(Limits {
        max_replay: 4,
        ..Limits::default()
    })
    .unwrap();
    state
        .admit(id(1), Credential::new([7; 32]), Origin(id(2)), &subscribe(id(3)))
        .unwrap();
    state
}

#[test]
fn retained_pin_revision_mismatch_is_atomic_and_does_not_stale_watches() {
    let mut state = admitted();
    state
        .apply_patch(
            id(1),
            0,
            1,
            &[Patch::Set {
                key: "a".into(),
                value: "one".into(),
            }],
            pin(1),
        )
        .unwrap();
    state.open_watch(id(1), id(5), 1).unwrap();

    let before = state.resync(id(1), 0).unwrap();
    assert_eq!(state.retain_pin(id(1)).unwrap(), Some(pin(1)));
    assert_eq!(
        state.apply_patch(
            id(1),
            1,
            2,
            &[Patch::Set {
                key: "b".into(),
                value: "two".into(),
            }],
            pin(9),
        ),
        Err(Error::RevisionMismatch)
    );

    assert_eq!(state.resync(id(1), 0).unwrap(), before);
    assert_eq!(state.retain_pin(id(1)).unwrap(), Some(pin(1)));
    assert_eq!(
        state.action(id(1), id(5), 1, 1),
        Err(Error::ActionSequenceMismatch)
    );

    state
        .apply_patch(
            id(1),
            1,
            2,
            &[Patch::Set {
                key: "b".into(),
                value: "two".into(),
            }],
            pin(2),
        )
        .unwrap();
    state
        .apply_patch(
            id(1),
            2,
            3,
            &[Patch::Set {
                key: "c".into(),
                value: "three".into(),
            }],
            pin(3),
        )
        .unwrap();

    assert_eq!(state.retain_pin(id(1)).unwrap(), Some(pin(3)));
    let replay = state.resync(id(1), 0).unwrap();
    assert_eq!(
        replay.iter().map(|revision| revision.revision).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(replay.last().unwrap().page.get("a"), Some(&"one".to_owned()));
    assert_eq!(replay.last().unwrap().page.get("b"), Some(&"two".to_owned()));
    assert_eq!(replay.last().unwrap().page.get("c"), Some(&"three".to_owned()));
    state.action(id(1), id(5), 3, 0).unwrap();
}
