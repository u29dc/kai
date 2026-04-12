use super::tool_spec;

#[test]
fn session_show_catalog_uses_pending_pairing_field() {
    let spec = tool_spec("session.show").expect("session.show tool");
    assert!(
        spec.output_fields
            .iter()
            .any(|field| field == "pendingPairing")
    );
    assert!(
        !spec
            .output_fields
            .iter()
            .any(|field| field == "pendingPairCode")
    );
}
