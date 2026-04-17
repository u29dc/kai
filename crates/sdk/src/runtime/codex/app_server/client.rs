use std::collections::{HashMap, VecDeque};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde::Serialize;
use serde_json::{Value as JsonValue, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command as TokioCommand};
use tokio::sync::{Mutex as AsyncMutex, broadcast, oneshot};

use crate::config::LoadedConfig;
use crate::error::{ErrorCode, KaiError, KaiResult};

use super::protocol::{
    ClientCapabilities, ClientInfo, InitializeParams, InitializeResponse, ResponseErrorPayload,
    ServerNotification, ThreadResumeParams, ThreadResumeResponse, ThreadStartParams,
    ThreadStartResponse, TurnInterruptParams, TurnStartParams, TurnStartResponse,
};

const STDERR_LINE_LIMIT: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ClientFingerprint {
    binary: String,
    service_name: Option<String>,
}

struct SharedClientSlot {
    fingerprint: ClientFingerprint,
    client: Arc<AppServerClient>,
}

static SHARED_CLIENT: OnceLock<AsyncMutex<Option<SharedClientSlot>>> = OnceLock::new();

pub struct AppServerClient {
    stdin: AsyncMutex<ChildStdin>,
    pending: Mutex<HashMap<u64, oneshot::Sender<KaiResult<JsonValue>>>>,
    notifications: broadcast::Sender<ServerNotification>,
    stderr_lines: Mutex<VecDeque<String>>,
    next_id: AtomicU64,
    closed: AtomicBool,
}

impl AppServerClient {
    pub async fn shared(config: &LoadedConfig) -> KaiResult<Arc<Self>> {
        let mutex = SHARED_CLIENT.get_or_init(|| AsyncMutex::new(None));
        let mut slot = mutex.lock().await;
        let fingerprint = ClientFingerprint {
            binary: config.values.runner.codex.binary.clone(),
            service_name: config.values.runner.codex.service_name.clone(),
        };

        if let Some(existing) = slot.as_ref()
            && existing.fingerprint == fingerprint
            && !existing.client.is_closed()
        {
            return Ok(existing.client.clone());
        }

        let client = Self::spawn(config).await?;
        *slot = Some(SharedClientSlot {
            fingerprint,
            client: client.clone(),
        });
        Ok(client)
    }

    pub async fn ephemeral(config: &LoadedConfig) -> KaiResult<Arc<Self>> {
        Self::spawn(config).await
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ServerNotification> {
        self.notifications.subscribe()
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub async fn initialize_smoke_test(config: &LoadedConfig) -> KaiResult<()> {
        let _client = Self::ephemeral(config).await?;
        Ok(())
    }

    pub async fn thread_start(&self, params: ThreadStartParams) -> KaiResult<ThreadStartResponse> {
        self.request("thread/start", params).await
    }

    pub async fn thread_resume(
        &self,
        params: ThreadResumeParams,
    ) -> KaiResult<ThreadResumeResponse> {
        self.request("thread/resume", params).await
    }

    pub async fn turn_start(&self, params: TurnStartParams) -> KaiResult<TurnStartResponse> {
        self.request("turn/start", params).await
    }

    pub async fn turn_interrupt(&self, params: TurnInterruptParams) -> KaiResult<()> {
        let _: JsonValue = self.request("turn/interrupt", params).await?;
        Ok(())
    }

    async fn spawn(config: &LoadedConfig) -> KaiResult<Arc<Self>> {
        let mut command = TokioCommand::new(&config.values.runner.codex.binary);
        command.arg("app-server");
        command.arg("--listen");
        command.arg("stdio://");
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        command.kill_on_drop(true);

        let mut child = command.spawn().map_err(|error| {
            KaiError::new(
                ErrorCode::RuntimeError,
                format!("failed to launch Codex App Server: {error}"),
            )
        })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            KaiError::new(
                ErrorCode::RuntimeError,
                "Codex App Server did not expose stdin",
            )
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            KaiError::new(
                ErrorCode::RuntimeError,
                "Codex App Server did not expose stdout",
            )
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            KaiError::new(
                ErrorCode::RuntimeError,
                "Codex App Server did not expose stderr",
            )
        })?;

        let (notifications, _) = broadcast::channel(256);
        let client = Arc::new(Self {
            stdin: AsyncMutex::new(stdin),
            pending: Mutex::new(HashMap::new()),
            notifications,
            stderr_lines: Mutex::new(VecDeque::new()),
            next_id: AtomicU64::new(1),
            closed: AtomicBool::new(false),
        });

        spawn_stdout_task(client.clone(), stdout);
        spawn_stderr_task(client.clone(), stderr);
        spawn_wait_task(client.clone(), child);
        client.initialize().await?;

        Ok(client)
    }

    async fn initialize(&self) -> KaiResult<()> {
        let result: InitializeResponse = self
            .request(
                "initialize",
                InitializeParams {
                    client_info: ClientInfo {
                        name: "kai".to_string(),
                        title: "kai".to_string(),
                        version: env!("CARGO_PKG_VERSION").to_string(),
                    },
                    capabilities: ClientCapabilities {
                        experimental_api: true,
                    },
                },
            )
            .await?;
        let _ = result;
        self.notify("initialized", JsonValue::Null).await
    }

    async fn request<T, P>(&self, method: &str, params: P) -> KaiResult<T>
    where
        T: serde::de::DeserializeOwned,
        P: Serialize,
    {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = oneshot::channel();
        self.pending
            .lock()
            .expect("pending lock poisoned")
            .insert(id, sender);

        let payload = json!({
            "id": id,
            "method": method,
            "params": params,
        });
        if let Err(error) = self.write_message(&payload).await {
            self.pending
                .lock()
                .expect("pending lock poisoned")
                .remove(&id);
            return Err(error);
        }

        let response = receiver
            .await
            .map_err(|_| self.server_closed_error("Codex App Server closed before responding"))??;
        serde_json::from_value::<T>(response).map_err(|error| {
            KaiError::new(
                ErrorCode::RuntimeError,
                format!("failed to decode Codex App Server `{method}` response: {error}"),
            )
        })
    }

    async fn notify(&self, method: &str, params: JsonValue) -> KaiResult<()> {
        let payload = if params.is_null() {
            json!({ "method": method })
        } else {
            json!({ "method": method, "params": params })
        };
        self.write_message(&payload).await
    }

    async fn write_message(&self, payload: &JsonValue) -> KaiResult<()> {
        if self.is_closed() {
            return Err(self.server_closed_error("Codex App Server is not running"));
        }

        let mut stdin = self.stdin.lock().await;
        let mut line = serde_json::to_vec(payload).map_err(|error| {
            KaiError::new(
                ErrorCode::RuntimeError,
                format!("failed to serialize Codex App Server message: {error}"),
            )
        })?;
        line.push(b'\n');
        stdin.write_all(&line).await.map_err(|error| {
            self.server_closed_error(&format!("failed to write to Codex App Server: {error}"))
        })?;
        stdin.flush().await.map_err(|error| {
            self.server_closed_error(&format!("failed to flush Codex App Server stdin: {error}"))
        })
    }

    fn complete_pending(&self, id: u64, result: KaiResult<JsonValue>) {
        if let Some(sender) = self
            .pending
            .lock()
            .expect("pending lock poisoned")
            .remove(&id)
        {
            let _ = sender.send(result);
        }
    }

    fn close_with_message(&self, message: String) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }

        let error = self.server_closed_error(&message);
        for sender in self
            .pending
            .lock()
            .expect("pending lock poisoned")
            .drain()
            .map(|(_, sender)| sender)
        {
            let _ = sender.send(Err(error.clone()));
        }
        let _ = self
            .notifications
            .send(ServerNotification::ServerExited { message });
    }

    fn push_stderr_line(&self, line: String) {
        let mut stderr_lines = self.stderr_lines.lock().expect("stderr lock poisoned");
        stderr_lines.push_back(line);
        while stderr_lines.len() > STDERR_LINE_LIMIT {
            let _ = stderr_lines.pop_front();
        }
    }

    fn server_closed_error(&self, message: &str) -> KaiError {
        let stderr = self
            .stderr_lines
            .lock()
            .expect("stderr lock poisoned")
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");

        let error = KaiError::new(ErrorCode::RuntimeError, message.to_string());
        if stderr.trim().is_empty() {
            error
        } else {
            error.with_hint(stderr)
        }
    }
}

fn spawn_stdout_task(client: Arc<AppServerClient>, stdout: tokio::process::ChildStdout) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    if let Err(error) = handle_server_line(client.clone(), &line).await {
                        client.push_stderr_line(error.message);
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    client.close_with_message(format!(
                        "failed to read Codex App Server stdout: {error}"
                    ));
                    return;
                }
            }
        }
    });
}

fn spawn_stderr_task(client: Arc<AppServerClient>, stderr: tokio::process::ChildStderr) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => client.push_stderr_line(line),
                Ok(None) => break,
                Err(error) => {
                    client.push_stderr_line(format!(
                        "failed to read Codex App Server stderr: {error}"
                    ));
                    return;
                }
            }
        }
    });
}

fn spawn_wait_task(client: Arc<AppServerClient>, mut child: Child) {
    tokio::spawn(async move {
        match child.wait().await {
            Ok(status) => client.close_with_message(format!("Codex App Server exited: {status}")),
            Err(error) => {
                client.close_with_message(format!("failed to wait for Codex App Server: {error}"))
            }
        }
    });
}

async fn handle_server_line(client: Arc<AppServerClient>, line: &str) -> KaiResult<()> {
    let value = serde_json::from_str::<JsonValue>(line).map_err(|error| {
        KaiError::new(
            ErrorCode::RuntimeError,
            format!("failed to parse Codex App Server message: {error}"),
        )
    })?;

    if let Some(id_value) = value.get("id").cloned() {
        if value.get("method").is_some()
            && value.get("result").is_none()
            && value.get("error").is_none()
        {
            let response = json!({
                "id": id_value,
                "error": { "message": "server-initiated requests are unsupported by kai" }
            });
            client.write_message(&response).await?;
            return Ok(());
        }

        let Some(id) = id_value.as_u64() else {
            return Ok(());
        };

        if let Some(error_value) = value.get("error") {
            let payload = serde_json::from_value::<ResponseErrorPayload>(error_value.clone())
                .unwrap_or(ResponseErrorPayload {
                    message: Some("Codex App Server returned an error".to_string()),
                    data: None,
                });
            let mut error = KaiError::new(
                ErrorCode::RuntimeError,
                payload
                    .message
                    .unwrap_or_else(|| "Codex App Server returned an error".to_string()),
            );
            if let Some(data) = payload.data {
                error = error.with_hint(data.to_string());
            }
            client.complete_pending(id, Err(error));
            return Ok(());
        }

        if let Some(result) = value.get("result") {
            client.complete_pending(id, Ok(result.clone()));
        }
        return Ok(());
    }

    let Some(method) = value.get("method").and_then(JsonValue::as_str) else {
        return Ok(());
    };
    let params = value.get("params").cloned().unwrap_or(JsonValue::Null);
    let notification = super::parse_notification(method, params);
    let _ = client.notifications.send(notification);
    Ok(())
}
