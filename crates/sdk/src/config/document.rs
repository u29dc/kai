use super::*;
use toml_edit::{Item, Table, value};

pub(super) fn load_or_create_document(path: &Path) -> KaiResult<DocumentMut> {
    if !path.exists() {
        return Ok(DocumentMut::new());
    }

    let raw = fs::read_to_string(path).map_err(|error| {
        KaiError::new(
            ErrorCode::ConfigError,
            format!("failed to read config file: {error}"),
        )
    })?;

    DocumentMut::from_str(&raw).map_err(|error| {
        KaiError::new(
            ErrorCode::ConfigError,
            format!("failed to initialize config document: {error}"),
        )
    })
}

pub(super) fn write_document(path: &Path, document: &mut DocumentMut) -> KaiResult<()> {
    prune_empty_tables(document.as_item_mut());

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            KaiError::new(
                ErrorCode::IoError,
                format!("failed to create config directory: {error}"),
            )
        })?;
    }

    fs::write(path, document.to_string()).map_err(|error| {
        KaiError::new(
            ErrorCode::IoError,
            format!("failed to write config file: {error}"),
        )
    })?;

    Ok(())
}

fn prune_empty_tables(item: &mut Item) -> bool {
    let Some(table) = item.as_table_like_mut() else {
        return false;
    };

    let keys = table
        .iter()
        .map(|(key, _)| key.to_string())
        .collect::<Vec<_>>();
    let mut empty_keys = Vec::new();

    for key in keys {
        let Some(child) = table.get_mut(&key) else {
            continue;
        };

        if prune_empty_tables(child) {
            empty_keys.push(key);
        }
    }

    for key in empty_keys {
        table.remove(&key);
    }

    table.is_empty()
}

pub(super) fn set_document_value(
    document: &mut DocumentMut,
    key: &str,
    raw_value: &str,
) -> KaiResult<()> {
    let mut segments = key.split('.').collect::<Vec<_>>();
    if segments.is_empty() {
        return Err(KaiError::invalid_argument("config key cannot be empty"));
    }

    let leaf = segments.pop().unwrap_or_default();
    let mut current = document.as_item_mut();
    for segment in segments {
        if !current.is_table() {
            *current = Item::Table(Table::new());
        }
        current = &mut current[segment];
    }

    *current = ensure_table_item(current);
    current[leaf] = parse_config_item(raw_value)?;
    Ok(())
}

pub(super) fn remove_document_value(document: &mut DocumentMut, key: &str) -> KaiResult<()> {
    let segments = key.split('.').collect::<Vec<_>>();
    if segments.is_empty() {
        return Err(KaiError::invalid_argument("config key cannot be empty"));
    }

    remove_item_value(document.as_item_mut(), &segments, key)?;
    Ok(())
}

fn remove_item_value(item: &mut Item, segments: &[&str], full_key: &str) -> KaiResult<bool> {
    if segments.is_empty() {
        return Err(KaiError::invalid_argument("config key cannot be empty"));
    }

    let segment = segments[0];
    let Some(table) = item.as_table_like_mut() else {
        return Err(KaiError::invalid_argument(format!(
            "unknown config key: {full_key}"
        )));
    };

    if segments.len() == 1 {
        if table.remove(segment).is_none() {
            return Err(
                KaiError::invalid_argument(format!("unknown config key: {full_key}"))
                    .with_hint("use `kai config show` to inspect available keys"),
            );
        }
        return Ok(table.is_empty());
    }

    let child = table.get_mut(segment).ok_or_else(|| {
        KaiError::invalid_argument(format!("unknown config key: {full_key}"))
            .with_hint("use `kai config show` to inspect available keys")
    })?;
    let child_empty = remove_item_value(child, &segments[1..], full_key)?;
    if child_empty {
        table.remove(segment);
    }
    Ok(table.is_empty())
}

fn ensure_table_item(item: &mut Item) -> Item {
    if item.is_table() {
        return item.clone();
    }

    Item::Table(Table::new())
}

fn parse_config_item(raw_value: &str) -> KaiResult<Item> {
    if let Ok(boolean) = raw_value.parse::<bool>() {
        return Ok(value(boolean));
    }
    if let Ok(integer) = raw_value.parse::<i64>() {
        return Ok(value(integer));
    }
    if let Ok(float) = raw_value.parse::<f64>() {
        return Ok(value(float));
    }

    Ok(value(raw_value))
}
