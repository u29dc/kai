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
fn config_mutation_rejects_unknown_keys() {
    let error = ensure_config_key_allowed("runner.codex.unknown")
        .expect_err("unknown keys should be rejected");
    assert!(error.message.contains("unknown or unsupported config key"));
}

#[test]
fn config_mutation_allows_high_capability_codex_override_keys() {
    ensure_config_key_allowed("runner.codex.override.approval_policy")
        .expect("approval policy override should be configurable");
    ensure_config_key_allowed("runner.codex.override.sandbox_mode")
        .expect("sandbox mode override should be configurable");
}

#[test]
fn config_document_validation_rejects_invalid_result_before_write() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let config_path = tempdir.path().join("config.toml");
    let mut document = DocumentMut::from_str(&build_default_config_file()).expect("document");
    set_document_value(&mut document, "workspaces.default", "missing").expect("set invalid");

    let error = validate_document_config(&document, &config_path)
        .expect_err("invalid workspace default should be rejected");

    assert!(error.message.contains("workspaces.default"));
    assert!(!config_path.exists());
}

#[test]
fn write_document_uses_private_file_permissions() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let config_path = tempdir.path().join("config.toml");
    let mut document = DocumentMut::from_str("[runner]\nprovider = \"codex\"\n").expect("document");

    write_document(&config_path, &mut document).expect("write document");

    let mode = crate::runtime_fs::read_unix_mode(&config_path).expect("mode");
    #[cfg(unix)]
    assert_eq!(mode, Some(0o600));
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
