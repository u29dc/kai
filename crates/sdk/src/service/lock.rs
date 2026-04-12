use super::*;

pub fn acquire_run_guard(config: &LoadedConfig) -> KaiResult<RunGuard> {
    let lock_path = run_lock_path(config);
    let parent = lock_path.parent().ok_or_else(|| {
        KaiError::new(
            ErrorCode::StateError,
            format!("invalid run lock path: {}", lock_path.display()),
        )
    })?;
    fs::create_dir_all(parent).map_err(io_error("create state directory"))?;

    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(io_error("open run lock"))?;

    if let Err(error) = file.try_lock_exclusive() {
        if is_lock_contended(&error) {
            let pid = read_lock_pid(&lock_path);
            let detail = pid
                .map(|value| format!("kai is already running (pid {value})"))
                .unwrap_or_else(|| "kai is already running".to_string());
            return Err(KaiError::blocked_prerequisite(detail)
                .with_hint("stop the existing `kai run` process or `kai service stop` first"));
        }

        return Err(KaiError::new(
            ErrorCode::StateError,
            format!("failed to lock run state: {error}"),
        ));
    }

    write_lock_pid(&mut file, std::process::id())?;
    Ok(RunGuard { file })
}

pub fn run_lock_status(config: &LoadedConfig) -> KaiResult<RunLockStatus> {
    let lock_path = run_lock_path(config);
    let recorded_pid = read_lock_pid(&lock_path);

    if !lock_path.exists() {
        return Ok(RunLockStatus {
            lock_path: lock_path.display().to_string(),
            locked: false,
            pid: None,
            stale: false,
        });
    }

    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(io_error("open run lock"))?;

    match file.try_lock_exclusive() {
        Ok(()) => {
            let (pid, stale) = match recorded_pid {
                Some(pid) if process_is_alive(pid) => (Some(pid), true),
                Some(_) => {
                    clear_lock_file(&mut file)?;
                    (None, false)
                }
                None => (None, false),
            };
            FileExt::unlock(&file).map_err(io_error("unlock run lock"))?;
            Ok(RunLockStatus {
                lock_path: lock_path.display().to_string(),
                locked: false,
                pid,
                stale,
            })
        }
        Err(error) if is_lock_contended(&error) => Ok(RunLockStatus {
            lock_path: lock_path.display().to_string(),
            locked: true,
            pid: recorded_pid,
            stale: false,
        }),
        Err(error) => Err(KaiError::new(
            ErrorCode::StateError,
            format!("failed to inspect run lock: {error}"),
        )),
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        let _ = self.file.set_len(0);
        let _ = self.file.seek(SeekFrom::Start(0));
        let _ = FileExt::unlock(&self.file);
    }
}

pub(super) fn run_lock_path(config: &LoadedConfig) -> PathBuf {
    state_paths(config).state_dir.join("run.lock")
}

fn is_lock_contended(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock
}

fn read_lock_pid(path: &Path) -> Option<u32> {
    let raw = fs::read_to_string(path).ok()?;
    raw.trim().parse::<u32>().ok()
}

fn write_lock_pid(file: &mut File, pid: u32) -> KaiResult<()> {
    clear_lock_file(file)?;
    file.write_all(pid.to_string().as_bytes())
        .map_err(io_error("write run lock"))?;
    file.write_all(b"\n").map_err(io_error("write run lock"))?;
    file.sync_data().map_err(io_error("sync run lock"))?;
    Ok(())
}

fn clear_lock_file(file: &mut File) -> KaiResult<()> {
    file.set_len(0).map_err(io_error("clear run lock"))?;
    file.seek(SeekFrom::Start(0))
        .map_err(io_error("seek run lock"))?;
    file.sync_data().map_err(io_error("sync run lock"))?;
    Ok(())
}

pub(super) fn process_is_alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}
