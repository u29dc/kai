use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use crate::error::{ErrorCode, KaiError, KaiResult};

const PRIVATE_DIR_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;
const PRIVATE_EXECUTABLE_MODE: u32 = 0o700;

pub fn ensure_private_dir(path: &Path) -> KaiResult<()> {
    fs::create_dir_all(path).map_err(io_error("create private directory"))?;
    set_mode_if_exists(path, PRIVATE_DIR_MODE, "harden private directory")
}

pub fn ensure_private_file(path: &Path) -> KaiResult<()> {
    ensure_parent_dir(path)?;

    #[cfg(unix)]
    {
        let _ = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(PRIVATE_FILE_MODE)
            .open(path)
            .map_err(io_error("create private file"))?;
    }

    #[cfg(not(unix))]
    {
        let _ = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(io_error("create private file"))?;
    }

    set_mode_if_exists(path, PRIVATE_FILE_MODE, "harden private file")
}

pub fn harden_private_file(path: &Path) -> KaiResult<()> {
    set_mode_if_exists(path, PRIVATE_FILE_MODE, "harden private file")
}

pub fn harden_private_executable(path: &Path) -> KaiResult<()> {
    set_mode_if_exists(path, PRIVATE_EXECUTABLE_MODE, "harden private executable")
}

pub fn write_private_file(path: &Path, contents: &[u8]) -> KaiResult<()> {
    write_mode_file(path, contents, PRIVATE_FILE_MODE, "write private file")
}

pub fn write_private_executable(path: &Path, contents: &[u8]) -> KaiResult<()> {
    write_mode_file(
        path,
        contents,
        PRIVATE_EXECUTABLE_MODE,
        "write private executable",
    )
}

pub fn read_unix_mode(path: &Path) -> KaiResult<Option<u32>> {
    #[cfg(unix)]
    {
        if !path.exists() {
            return Ok(None);
        }

        let metadata = fs::metadata(path).map_err(io_error("inspect filesystem metadata"))?;
        Ok(Some(metadata.permissions().mode() & 0o777))
    }

    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(None)
    }
}

pub fn octal_mode(mode: u32) -> String {
    format!("{:03o}", mode & 0o777)
}

fn write_mode_file(path: &Path, contents: &[u8], mode: u32, action: &'static str) -> KaiResult<()> {
    ensure_parent_dir(path)?;

    #[cfg(unix)]
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(mode)
        .open(path)
        .map_err(io_error(action))?;

    #[cfg(not(unix))]
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(io_error(action))?;

    file.write_all(contents).map_err(io_error(action))?;
    file.sync_all().map_err(io_error(action))?;
    set_mode_if_exists(path, mode, action)
}

fn ensure_parent_dir(path: &Path) -> KaiResult<()> {
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)?;
    }
    Ok(())
}

fn set_mode_if_exists(path: &Path, mode: u32, action: &'static str) -> KaiResult<()> {
    if !path.exists() {
        return Ok(());
    }

    #[cfg(unix)]
    {
        let permissions = fs::Permissions::from_mode(mode);
        fs::set_permissions(path, permissions).map_err(io_error(action))?;
    }

    Ok(())
}

fn io_error(action: &'static str) -> impl Fn(std::io::Error) -> KaiError {
    move |error| KaiError::new(ErrorCode::IoError, format!("failed to {action}: {error}"))
}
