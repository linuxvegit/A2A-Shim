use a2a_shim_core::wire::card::{
    AgentCapabilities, AgentCard, AgentCardMetadata, ConversationsCapability,
};

#[test]
fn card_canonical_shape_matches_spec_2_10() {
    let card = AgentCard {
        name: "claude-code-sidecar".into(),
        description: "Claude Agent exposed as an A2A endpoint".into(),
        version: "0.1.0".into(),
        url: "http://127.0.0.1:7001/".into(),
        capabilities: AgentCapabilities {
            streaming: true,
            push_notifications: false,
            state_transition_history: true,
        },
        default_input_modes: vec!["text/plain".into()],
        default_output_modes: vec!["text/plain".into()],
        skills: vec![],
        metadata: Some(AgentCardMetadata {
            conversations: Some(ConversationsCapability {
                supported: true,
                metadata_key: "x-a2a-shim/conversation".into(),
                context_id_alias: true,
                max_active: 64,
                idle_secs: 86400,
            }),
        }),
    };
    let v = serde_json::to_value(&card).unwrap();
    assert_eq!(v["capabilities"]["streaming"], true);
    assert_eq!(v["capabilities"]["pushNotifications"], false);
    assert_eq!(v["capabilities"]["stateTransitionHistory"], true);
    let conv = &v["metadata"]["x-a2a-shim/conversations"];
    assert_eq!(conv["supported"], true);
    assert_eq!(conv["metadataKey"], "x-a2a-shim/conversation");
    assert_eq!(conv["contextIdAlias"], true);
    assert_eq!(conv["maxActive"], 64);
    assert_eq!(conv["idleSecs"], 86400);
}
