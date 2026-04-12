use super::*;

#[test]
fn build_default_config_contains_expected_sections() {
    let config = build_default_config_file();
    assert!(config.contains("[agent]"));
    assert!(config.contains("[channel.telegram]"));
    assert!(config.contains("[paths]"));
}

#[test]
fn expand_home_resolves_tilde_prefix() {
    let home = std::env::var("HOME").expect("HOME must be available in tests");
    let path = expand_home("~/tmp/kai-test");
    assert_eq!(path, PathBuf::from(home).join("tmp").join("kai-test"));
}

#[test]
fn remove_document_value_prunes_empty_parent_tables() {
    let mut document = DocumentMut::from_str(
        r#"
[channel.telegram]
owner_user_id = 123
"#,
    )
    .expect("document");

    remove_document_value(&mut document, "channel.telegram.owner_user_id").expect("remove value");

    let rendered = document.to_string();
    assert!(!rendered.contains("[channel]"));
    assert!(!rendered.contains("[channel.telegram]"));
}
