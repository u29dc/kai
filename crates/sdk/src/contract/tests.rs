use super::{tool_catalog, tool_spec};

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

#[test]
fn session_catalog_includes_side_query_state() {
    let spec = tool_spec("session.show").expect("session.show tool");
    assert!(
        spec.output_fields
            .iter()
            .any(|field| field == "activeSideQuery")
    );
}

#[test]
fn catalog_hides_deprecated_alias_commands() {
    let catalog = tool_catalog();
    let names = catalog
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>();

    assert!(!names.contains(&"context.check"));
    assert!(!names.contains(&"session.reset"));
    assert!(!names.contains(&"workspace.list"));
}
