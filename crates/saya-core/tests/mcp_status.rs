use saya_core::contract::{EventPayload, McpServerState, McpServerStatus, McpTransport};

#[test]
fn mcp_status_events_serialize_only_host_catalog_identity_and_lifecycle() {
    let payload = EventPayload::McpStatus {
        servers: vec![McpServerStatus {
            server: "fixture".into(),
            transport: McpTransport::Stdio,
            state: McpServerState::Connected,
            tools: 3,
        }],
    };
    let encoded = serde_json::to_value(&payload).unwrap();
    assert_eq!(
        encoded,
        serde_json::json!({"type":"mcp.status","data":{"servers":[{
            "server":"fixture","transport":"stdio","state":"connected","tools":3
        }]}})
    );
    assert_eq!(
        serde_json::from_value::<EventPayload>(encoded).unwrap(),
        payload
    );
}
