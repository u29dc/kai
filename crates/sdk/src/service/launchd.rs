use super::*;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
use self::macos::{
    launch_agent_plist_path, launch_target_label, launchd_status, run_launchctl, seed_secrets,
};

pub fn service_status(config: &LoadedConfig) -> KaiResult<ServiceStatus> {
    let lock = lock::run_lock_status(config)?;
    let stdout_path = service_stdout_path(config).display().to_string();
    let stderr_path = service_stderr_path(config).display().to_string();

    #[cfg(target_os = "macos")]
    {
        let plist_path = launch_agent_plist_path()?;
        let installed = plist_path.exists();
        let launchd = launchd_status()?;
        let active_mode = if launchd.loaded && launchd.pid.is_some() {
            "service"
        } else if lock.locked {
            "manual"
        } else {
            "stopped"
        };

        Ok(ServiceStatus {
            platform: env::consts::OS.to_string(),
            label: MAC_LABEL.to_string(),
            installed,
            loaded: launchd.loaded,
            running: launchd.pid.is_some() || lock.locked,
            pid: launchd.pid.or(lock.pid.filter(|_| lock.locked)),
            active_mode: active_mode.to_string(),
            plist_path: Some(plist_path.display().to_string()),
            stdout_path,
            stderr_path,
            lock,
        })
    }

    #[cfg(not(target_os = "macos"))]
    {
        Ok(ServiceStatus {
            platform: env::consts::OS.to_string(),
            label: MAC_LABEL.to_string(),
            installed: false,
            loaded: false,
            running: lock.locked,
            pid: lock.pid.filter(|_| lock.locked),
            active_mode: if lock.locked {
                "manual".to_string()
            } else {
                "stopped".to_string()
            },
            plist_path: None,
            stdout_path,
            stderr_path,
            lock,
        })
    }
}

pub fn service_start(config: &LoadedConfig) -> KaiResult<ServiceActionOutput> {
    let status = service_status(config)?;
    if status.running {
        let detail = status
            .pid
            .map(|pid| format!("kai is already running (pid {pid})"))
            .unwrap_or_else(|| "kai is already running".to_string());
        return Err(KaiError::blocked_prerequisite(detail)
            .with_hint("run `kai service restart` or `kai service stop` before starting again"));
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = config;
        return Err(KaiError::blocked_prerequisite(
            "background service management is only implemented for macOS right now",
        ));
    }

    #[cfg(target_os = "macos")]
    {
        validate_service_runtime_prerequisites(config)?;
        seed_secrets(config)?;

        let binary_path = runtime_binary_path(config);
        if !binary_path.is_file() {
            return Err(KaiError::blocked_prerequisite(format!(
                "installed kai binary is missing at {}",
                binary_path.display()
            ))
            .with_hint(
                "run `bun run build` so `~/.tools/kai/kai` exists before starting the service",
            ));
        }

        let plist_path = launch_agent_plist_path()?;
        let launch_agents_dir = plist_path.parent().ok_or_else(|| {
            KaiError::new(
                ErrorCode::StateError,
                format!("invalid plist path: {}", plist_path.display()),
            )
        })?;
        let stdout_path = service_stdout_path(config);
        let stderr_path = service_stderr_path(config);
        let state_dir = state_paths(config).state_dir;
        let runner_path = service_runner_path(config);

        fs::create_dir_all(launch_agents_dir).map_err(io_error("create LaunchAgents directory"))?;
        ensure_private_dir(Path::new(&config.values.paths.root_app))?;
        ensure_private_dir(&state_dir)?;
        ensure_private_dir(
            stdout_path
                .parent()
                .ok_or_else(|| KaiError::new(ErrorCode::StateError, "invalid stdout path"))?,
        )?;
        ensure_private_dir(
            stderr_path
                .parent()
                .ok_or_else(|| KaiError::new(ErrorCode::StateError, "invalid stderr path"))?,
        )?;
        ensure_private_dir(
            runner_path
                .parent()
                .ok_or_else(|| KaiError::new(ErrorCode::StateError, "invalid runner path"))?,
        )?;
        ensure_private_file(&stdout_path)?;
        ensure_private_file(&stderr_path)?;

        let runner = render_service_runner(config, &binary_path);
        write_private_executable(&runner_path, runner.as_bytes())?;

        let plist = render_macos_plist(config, &runner_path);
        fs::write(&plist_path, plist).map_err(io_error("write launch agent plist"))?;
        harden_private_file(&plist_path)?;

        let _ = run_launchctl(["bootout", &launch_target_label()?]);
        let gui_target = format!("gui/{}", macos::current_uid()?);
        let plist_path_string = plist_path.display().to_string();

        let bootstrap =
            run_launchctl(["bootstrap", gui_target.as_str(), plist_path_string.as_str()])?;
        if !bootstrap.status.success() {
            let stderr = String::from_utf8_lossy(&bootstrap.stderr);
            if !(stderr.contains("already loaded") || stderr.contains("already exists")) {
                return Err(command_error(
                    "launchctl bootstrap",
                    &stderr,
                    Some("check the generated LaunchAgent plist and retry"),
                ));
            }
        }

        wait_for_service_running(config)?;
        ensure_private_file(&stdout_path)?;
        ensure_private_file(&stderr_path)?;

        Ok(ServiceActionOutput {
            action: "started".to_string(),
            status: service_status(config)?,
        })
    }
}

pub fn service_stop(config: &LoadedConfig) -> KaiResult<ServiceActionOutput> {
    let status = service_status(config)?;

    #[cfg(not(target_os = "macos"))]
    {
        let _ = config;
        return Err(KaiError::blocked_prerequisite(
            "background service management is only implemented for macOS right now",
        ));
    }

    #[cfg(target_os = "macos")]
    {
        if !status.loaded {
            if status.lock.locked {
                let detail = status
                    .lock
                    .pid
                    .map(|pid| format!("kai is running in the foreground (pid {pid})"))
                    .unwrap_or_else(|| "kai is running in the foreground".to_string());
                return Err(KaiError::blocked_prerequisite(detail)
                    .with_hint("stop the foreground `kai run` process directly"));
            }

            return Ok(ServiceActionOutput {
                action: "already_stopped".to_string(),
                status,
            });
        }

        let target = launch_target_label()?;
        let output = run_launchctl(["bootout", target.as_str()])?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.contains("No such process")
                && !stderr.contains("Could not find specified service")
            {
                return Err(command_error(
                    "launchctl bootout",
                    &stderr,
                    Some("inspect the LaunchAgent status and retry"),
                ));
            }
        }

        wait_for_service_stopped(config)?;

        Ok(ServiceActionOutput {
            action: "stopped".to_string(),
            status: service_status(config)?,
        })
    }
}

pub fn service_restart(config: &LoadedConfig) -> KaiResult<ServiceActionOutput> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = config;
        return Err(KaiError::blocked_prerequisite(
            "background service management is only implemented for macOS right now",
        ));
    }

    #[cfg(target_os = "macos")]
    {
        let status = service_status(config)?;
        if status.lock.locked && !status.loaded {
            let detail = status
                .lock
                .pid
                .map(|pid| format!("kai is running in the foreground (pid {pid})"))
                .unwrap_or_else(|| "kai is running in the foreground".to_string());
            return Err(KaiError::blocked_prerequisite(detail).with_hint(
                "stop the foreground `kai run` process before using `kai service restart`",
            ));
        }

        validate_service_runtime_prerequisites(config)?;
        let _ = service_stop(config)?;
        service_start(config).map(|mut output| {
            output.action = "restarted".to_string();
            output
        })
    }
}

pub fn service_uninstall(config: &LoadedConfig) -> KaiResult<ServiceActionOutput> {
    let status = service_status(config)?;

    #[cfg(not(target_os = "macos"))]
    {
        let _ = config;
        return Err(KaiError::blocked_prerequisite(
            "background service management is only implemented for macOS right now",
        ));
    }

    #[cfg(target_os = "macos")]
    {
        if status.lock.locked && !status.loaded {
            let detail = status
                .lock
                .pid
                .map(|pid| format!("kai is running in the foreground (pid {pid})"))
                .unwrap_or_else(|| "kai is running in the foreground".to_string());
            return Err(KaiError::blocked_prerequisite(detail).with_hint(
                "stop the foreground `kai run` process before uninstalling the service",
            ));
        }

        let _ = service_stop(config)?;
        if let Some(plist_path) = service_status(config)?.plist_path {
            let path = PathBuf::from(plist_path);
            if path.exists() {
                fs::remove_file(&path).map_err(io_error("remove launch agent plist"))?;
            }
        }
        let runner_path = service_runner_path(config);
        if runner_path.exists() {
            fs::remove_file(&runner_path).map_err(io_error("remove service runner"))?;
        }

        Ok(ServiceActionOutput {
            action: "uninstalled".to_string(),
            status: service_status(config)?,
        })
    }
}

fn wait_for_service_running(config: &LoadedConfig) -> KaiResult<()> {
    wait_for_service_condition(config, |status| status.loaded && status.running).map(|_| ())
}

fn wait_for_service_stopped(config: &LoadedConfig) -> KaiResult<()> {
    wait_for_service_condition(config, |status| !status.loaded && !status.running).map(|_| ())
}

fn wait_for_service_condition(
    config: &LoadedConfig,
    predicate: impl Fn(&ServiceStatus) -> bool,
) -> KaiResult<ServiceStatus> {
    let started_at = Instant::now();
    loop {
        let status = service_status(config)?;
        if predicate(&status) {
            return Ok(status);
        }

        if started_at.elapsed() >= SERVICE_SETTLE_TIMEOUT {
            return Err(KaiError::new(
                ErrorCode::RuntimeError,
                "service state did not settle before timeout",
            )
            .with_hint("run `kai service status` and inspect the service logs"));
        }

        thread::sleep(SERVICE_SETTLE_POLL);
    }
}

#[cfg(target_os = "macos")]
pub(super) fn render_macos_plist(config: &LoadedConfig, runner_path: &Path) -> String {
    macos::render_macos_plist(config, runner_path)
}

#[cfg(target_os = "macos")]
pub(super) fn render_service_runner(config: &LoadedConfig, binary_path: &Path) -> String {
    macos::render_service_runner(config, binary_path)
}
