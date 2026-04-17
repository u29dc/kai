use super::*;

#[test]
fn build_default_config_contains_expected_sections() {
    let config = build_default_config_file();
    assert!(config.contains("[agent]"));
    assert!(config.contains("[channel.telegram]"));
    assert!(config.contains("[channel.telegram.progress]"));
    assert!(config.contains("[paths]"));
    assert!(config.contains("[context_files]"));
    assert!(config.contains("[runner]"));
    assert!(config.contains("provider = \"codex\""));
    assert!(config.contains("[runner.codex]"));
    assert!(config.contains("[workspaces]"));
    assert!(config.contains("[workspaces.main]"));
    assert!(!config.contains("root_work"));
    assert!(!config.contains("todo ="));
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

#[test]
fn migrate_config_to_workspaces_rewrites_legacy_keys() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let root_app = tempdir.path().join("kai-home");
    let legacy_root_work = tempdir.path().join("vault");
    let config_path = tempdir.path().join("config.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"
[paths]
root_app = "{}"
root_work = "{}"

[context_files]
soul = "{}/SOUL.md"
memory = "{}/MEMORY.md"
todo = "{}/TODO.md"
"#,
            root_app.display(),
            legacy_root_work.display(),
            root_app.display(),
            root_app.display(),
            root_app.display()
        ),
    )
    .expect("write legacy config");
    let result = migrate_config_to_workspaces_at(&config_path).expect("migrate config");
    let rendered = std::fs::read_to_string(&config_path).expect("read migrated config");

    assert!(result.migrated);
    assert_eq!(result.default_workspace_id, "vault");
    assert!(result.backup_path.is_some());
    assert!(rendered.contains("[workspaces]"));
    assert!(rendered.contains("default = \"vault\""));
    assert!(rendered.contains("[workspaces.vault]"));
    assert!(rendered.contains(&format!("path = \"{}\"", legacy_root_work.display())));
    assert!(!rendered.contains("root_work"));
    assert!(!rendered.contains("todo ="));
}
