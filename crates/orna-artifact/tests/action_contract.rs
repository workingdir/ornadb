use orna_artifact::client_plan::{
    ActionClientPlan, ActionOperationNode, ActionTargetDomain, ClientExpressionNode,
};
use orna_core::{
    CallSiteId, CatalogueRevisionId, FunctionId, ParameterId, SourceRevisionId, TypeId,
    revision::RevisionPair,
};

#[test]
fn action_plan_round_trips_multiple_canonical_arguments_without_losing_identity() {
    let target = FunctionId::from_bytes([0x21; 16]);
    let revision = RevisionPair::new(
        SourceRevisionId::from_bytes([0x31; 16]),
        CatalogueRevisionId::from_bytes([0x32; 16]),
    );
    let call_site = CallSiteId::from_bytes([0x33; 16]);
    let result_type = TypeId::from_bytes([0x34; 16]);
    let arguments = vec![
        (
            ParameterId::from_bytes([0x11; 16]),
            ClientExpressionNode::String {
                value: "owner".to_owned(),
            },
        ),
        (
            ParameterId::from_bytes([0x22; 16]),
            ClientExpressionNode::Integer { value: -7 },
        ),
    ];
    let operation = ActionOperationNode::new(
        ActionTargetDomain::Server,
        target,
        revision,
        call_site,
        arguments.clone(),
        result_type,
    );
    let plan = ActionClientPlan::new(operation.clone());

    let encoded = plan.encode().expect("the action plan encodes");
    let decoded = ActionClientPlan::decode(&encoded).expect("the action plan decodes");

    assert_eq!(decoded.operation(), &operation);
    assert_eq!(decoded.operation().target(), target);
    assert_eq!(decoded.operation().target_revision(), revision);
    assert_eq!(decoded.operation().call_site(), call_site);
    assert_eq!(decoded.operation().result_type(), result_type);
    assert_eq!(decoded.operation().arguments(), arguments.as_slice());
}
