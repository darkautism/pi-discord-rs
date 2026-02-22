use super::{AgentEvent, AgentState, AiAgent, ModelInfo};
use crate::agent::runtime;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::{broadcast, oneshot, Mutex, OnceCell, RwLock};
use tracing::{error, info, warn};

static COPILOT_RUNTIME: OnceCell<Arc<CopilotRuntime>> = OnceCell::const_new();

#[derive(Clone, Debug, Default)]
struct SessionInfoCache {
    models: Vec<ModelInfo>,
    current_model: Option<String>,
}

#[derive(Clone, Debug)]
struct SessionBootstrap {
    session_id: String,
    info: SessionInfoCache,
}

#[derive(Debug, Clone, PartialEq)]
enum SessionUpdateAction {
    MessageUpdate {
        thinking: String,
        text: String,
        is_delta: bool,
        id: Option<String>,
    },
    ToolStart {
        id: String,
        name: String,
    },
    ToolUpdate {
        id: String,
        output: String,
    },
    Ignore,
}

struct CopilotRuntime {
    stdin: Mutex<ChildStdin>,
    child: Mutex<Child>,
    pending: Mutex<HashMap<u64, oneshot::Sender<anyhow::Result<Value>>>>,
    session_senders: RwLock<HashMap<String, broadcast::Sender<AgentEvent>>>,
    session_info: RwLock<HashMap<String, SessionInfoCache>>,
    next_id: AtomicU64,
}

impl CopilotRuntime {
    async fn get() -> anyhow::Result<Arc<Self>> {
        let runtime = COPILOT_RUNTIME
            .get_or_try_init(|| async {
                let runtime = Self::spawn().await?;
                runtime
                    .request("initialize", json!({ "protocolVersion": 1 }))
                    .await?;
                Ok::<Arc<Self>, anyhow::Error>(runtime)
            })
            .await?;
        Ok(Arc::clone(runtime))
    }

    async fn spawn() -> anyhow::Result<Arc<Self>> {
        let copilot_bin = runtime::resolve_binary_with_env("COPILOT_BINARY", "copilot");
        let current_path = std::env::var("PATH").unwrap_or_default();
        let mut cmd = Command::new(&copilot_bin);
        cmd.arg("--acp")
            .arg("--allow-all-tools")
            .arg("--allow-all-paths")
            .arg("--allow-all-urls")
            .env("PATH", runtime::build_augmented_path(&current_path))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("Copilot ACP stdin not available"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Copilot ACP stdout not available"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("Copilot ACP stderr not available"))?;

        let runtime = Arc::new(Self {
            stdin: Mutex::new(stdin),
            child: Mutex::new(child),
            pending: Mutex::new(HashMap::new()),
            session_senders: RwLock::new(HashMap::new()),
            session_info: RwLock::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        });

        Self::spawn_stdout_reader(Arc::clone(&runtime), stdout);
        Self::spawn_stderr_logger(stderr);
        info!("✅ Copilot ACP backend started");
        Ok(runtime)
    }

    fn spawn_stdout_reader(runtime: Arc<Self>, stdout: ChildStdout) {
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            while let Ok(n) = reader.read_line(&mut line).await {
                if n == 0 {
                    break;
                }
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    match serde_json::from_str::<Value>(trimmed) {
                        Ok(msg) => runtime.handle_message(msg).await,
                        Err(e) => warn!("Copilot ACP invalid JSON: {}", e),
                    }
                }
                line.clear();
            }
            error!("❌ Copilot ACP stdout closed");
        });
    }

    fn spawn_stderr_logger(stderr: ChildStderr) {
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            while let Ok(n) = reader.read_line(&mut line).await {
                if n == 0 {
                    break;
                }
                let msg = line.trim();
                if !msg.is_empty() {
                    warn!("copilot(acp): {}", msg);
                }
                line.clear();
            }
        });
    }

    async fn ensure_alive(&self) -> anyhow::Result<()> {
        let mut child = self.child.lock().await;
        if let Some(status) = child.try_wait()? {
            anyhow::bail!("Copilot ACP exited: {}", status);
        }
        Ok(())
    }

    async fn handle_message(&self, msg: Value) {
        if let Some(method) = msg.get("method").and_then(Value::as_str) {
            match method {
                "session/update" => self.handle_session_update(&msg).await,
                "session/request_permission" => self.handle_permission_request(&msg).await,
                _ => {}
            }
            return;
        }

        if let Some(id) = msg.get("id").and_then(Value::as_u64) {
            let tx = self.pending.lock().await.remove(&id);
            if let Some(tx) = tx {
                if let Some(err) = msg.get("error") {
                    let _ = tx.send(Err(anyhow::anyhow!(Self::error_text(err))));
                } else {
                    let _ = tx.send(Ok(msg.get("result").cloned().unwrap_or(Value::Null)));
                }
            }
        }
    }

    async fn handle_permission_request(&self, msg: &Value) {
        let id = match msg.get("id").and_then(Value::as_u64) {
            Some(v) => v,
            None => return,
        };

        let option_id = Self::permission_option_id(msg);

        if let Some(option_id) = option_id {
            let response = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "optionId": option_id }
            });
            if let Err(e) = self.send_raw(&response).await {
                warn!("Failed to auto-respond permission request: {}", e);
            }
        }
    }

    fn permission_option_id(msg: &Value) -> Option<String> {
        msg["params"]["options"].as_array().and_then(|options| {
            options
                .iter()
                .find_map(|opt| {
                    let id = opt.get("optionId")?.as_str()?;
                    if id.contains("allow_always") {
                        Some(id.to_string())
                    } else {
                        None
                    }
                })
                .or_else(|| {
                    options
                        .iter()
                        .find_map(|opt| opt.get("optionId")?.as_str().map(|s| s.to_string()))
                })
        })
    }

    async fn handle_session_update(&self, msg: &Value) {
        let session_id = match msg["params"]["sessionId"].as_str() {
            Some(v) => v,
            None => return,
        };

        let tx = {
            let sessions = self.session_senders.read().await;
            sessions.get(session_id).cloned()
        };
        let Some(tx) = tx else {
            return;
        };

        let update = &msg["params"]["update"];
        match Self::parse_session_update(update) {
            SessionUpdateAction::MessageUpdate {
                thinking,
                text,
                is_delta,
                id,
            } => {
                let _ = tx.send(AgentEvent::MessageUpdate {
                    thinking,
                    text,
                    is_delta,
                    id,
                });
            }
            SessionUpdateAction::ToolStart { id, name } => {
                let _ = tx.send(AgentEvent::ToolExecutionStart { id, name });
            }
            SessionUpdateAction::ToolUpdate { id, output } => {
                let _ = tx.send(AgentEvent::ToolExecutionUpdate { id, output });
            }
            SessionUpdateAction::Ignore => {}
        }
    }

    fn parse_session_update(update: &Value) -> SessionUpdateAction {
        let update_type = update["sessionUpdate"].as_str().unwrap_or("");
        match update_type {
            "agent_thought_chunk" => {
                if let Some(text) = Self::update_text(update) {
                    SessionUpdateAction::MessageUpdate {
                        thinking: text,
                        text: "".to_string(),
                        is_delta: true,
                        id: None,
                    }
                } else {
                    SessionUpdateAction::Ignore
                }
            }
            "agent_message_chunk" => {
                if let Some(text) = Self::update_text(update) {
                    SessionUpdateAction::MessageUpdate {
                        thinking: "".to_string(),
                        text,
                        is_delta: true,
                        id: None,
                    }
                } else {
                    SessionUpdateAction::Ignore
                }
            }
            "tool_call" => {
                let id = update["toolCallId"].as_str().unwrap_or("tool").to_string();
                let status = update["status"].as_str().unwrap_or("");
                let title = update["title"]
                    .as_str()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "Tool Call".to_string());
                if status == "pending" || status == "running" {
                    SessionUpdateAction::ToolStart { id, name: title }
                } else {
                    SessionUpdateAction::Ignore
                }
            }
            "tool_call_update" => {
                let id = update["toolCallId"].as_str().unwrap_or("tool").to_string();
                let status = update["status"].as_str().unwrap_or("");
                let output = if !update["rawOutput"].is_null() {
                    Self::value_text(&update["rawOutput"])
                } else {
                    status.to_string()
                };
                if output.is_empty() {
                    SessionUpdateAction::Ignore
                } else {
                    SessionUpdateAction::ToolUpdate { id, output }
                }
            }
            _ => SessionUpdateAction::Ignore,
        }
    }

    fn update_text(update: &Value) -> Option<String> {
        update
            .get("content")
            .and_then(|c| c.get("text"))
            .and_then(Value::as_str)
            .map(|s| s.to_string())
            .or_else(|| {
                update
                    .get("text")
                    .and_then(Value::as_str)
                    .map(|s| s.to_string())
            })
    }

    fn value_text(value: &Value) -> String {
        if let Some(s) = value.as_str() {
            s.to_string()
        } else {
            serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
        }
    }

    fn error_text(err: &Value) -> String {
        let message = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Unknown error");
        match err.get("data") {
            Some(data) if !data.is_null() => format!("{}: {}", message, data),
            _ => message.to_string(),
        }
    }

    async fn send_raw(&self, payload: &Value) -> anyhow::Result<()> {
        let line = serde_json::to_string(payload)?;
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
    }

    async fn request(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        self.ensure_alive().await?;

        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let payload = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });
        self.send_raw(&payload).await?;

        match tokio::time::timeout(Duration::from_secs(300), rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => anyhow::bail!("ACP response channel dropped: {}", method),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                anyhow::bail!("ACP request timeout: {}", method);
            }
        }
    }

    fn parse_session_bootstrap(result: Value) -> anyhow::Result<SessionBootstrap> {
        let session_id = result["sessionId"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing sessionId in ACP response"))?
            .to_string();

        let models = result["models"]["availableModels"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| {
                        let id = m.get("modelId")?.as_str()?;
                        let label = m
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or(id)
                            .to_string();
                        Some(ModelInfo {
                            provider: "copilot".to_string(),
                            id: id.to_string(),
                            label,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let current_model = result["models"]["currentModelId"]
            .as_str()
            .map(|s| s.to_string());

        Ok(SessionBootstrap {
            session_id,
            info: SessionInfoCache {
                models,
                current_model,
            },
        })
    }

    async fn create_session(&self, cwd: &str) -> anyhow::Result<SessionBootstrap> {
        let result = self
            .request("session/new", json!({ "cwd": cwd, "mcpServers": [] }))
            .await?;
        let bootstrap = Self::parse_session_bootstrap(result)?;
        self.session_info
            .write()
            .await
            .insert(bootstrap.session_id.clone(), bootstrap.info.clone());
        Ok(bootstrap)
    }

    async fn load_session(&self, session_id: &str, cwd: &str) -> anyhow::Result<SessionBootstrap> {
        let result = self
            .request(
                "session/load",
                json!({ "sessionId": session_id, "cwd": cwd, "mcpServers": [] }),
            )
            .await?;
        let bootstrap = Self::parse_session_bootstrap(result)?;
        self.session_info
            .write()
            .await
            .insert(bootstrap.session_id.clone(), bootstrap.info.clone());
        Ok(bootstrap)
    }

    async fn cached_session_info(&self, session_id: &str) -> Option<SessionInfoCache> {
        self.session_info.read().await.get(session_id).cloned()
    }

    async fn register_session_sender(&self, session_id: &str, tx: broadcast::Sender<AgentEvent>) {
        self.session_senders
            .write()
            .await
            .insert(session_id.to_string(), tx);
    }

    async fn prompt(&self, session_id: &str, message: &str) -> anyhow::Result<()> {
        self.request(
            "session/prompt",
            json!({
                "sessionId": session_id,
                "prompt": [{ "type": "text", "text": message }]
            }),
        )
        .await?;
        Ok(())
    }

    async fn set_model(&self, session_id: &str, model_id: &str) -> anyhow::Result<()> {
        self.request(
            "session/set_model",
            json!({
                "sessionId": session_id,
                "modelId": model_id
            }),
        )
        .await?;

        let mut info_map = self.session_info.write().await;
        let entry = info_map.entry(session_id.to_string()).or_default();
        entry.current_model = Some(model_id.to_string());
        Ok(())
    }
}

pub struct CopilotAgent {
    runtime: Arc<CopilotRuntime>,
    channel_id: u64,
    session_id: String,
    event_tx: broadcast::Sender<AgentEvent>,
    message_count: AtomicU64,
    models: Arc<RwLock<Vec<ModelInfo>>>,
    current_model: Arc<RwLock<Option<String>>>,
}

impl CopilotAgent {
    pub async fn new(
        channel_id: u64,
        existing_sid: Option<String>,
        model_opt: Option<(String, String)>,
    ) -> anyhow::Result<Arc<Self>> {
        let runtime = CopilotRuntime::get().await?;
        let cwd = std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .to_string_lossy()
            .to_string();

        let (bootstrap, loaded_existing) = if let Some(sid) = existing_sid {
            match runtime.load_session(&sid, &cwd).await {
                Ok(info) => (info, true),
                Err(e) if e.to_string().contains("already loaded") => {
                    let cached = runtime.cached_session_info(&sid).await.unwrap_or_default();
                    (
                        SessionBootstrap {
                            session_id: sid,
                            info: cached,
                        },
                        true,
                    )
                }
                Err(e) => {
                    warn!("Failed to load Copilot session, creating new one: {}", e);
                    (runtime.create_session(&cwd).await?, false)
                }
            }
        } else {
            (runtime.create_session(&cwd).await?, false)
        };

        let (event_tx, _) = broadcast::channel(1000);
        runtime
            .register_session_sender(&bootstrap.session_id, event_tx.clone())
            .await;

        let agent = Arc::new(Self {
            runtime,
            channel_id,
            session_id: bootstrap.session_id.clone(),
            event_tx,
            message_count: AtomicU64::new(if loaded_existing { 1 } else { 0 }),
            models: Arc::new(RwLock::new(bootstrap.info.models.clone())),
            current_model: Arc::new(RwLock::new(bootstrap.info.current_model.clone())),
        });

        if let Some((provider, model_id)) = model_opt {
            if provider == "copilot" && !model_id.is_empty() {
                if let Err(e) = agent.set_model(&provider, &model_id).await {
                    warn!("Failed to restore Copilot model preference: {}", e);
                }
            }
        }

        Ok(agent)
    }

    pub fn session_id(&self) -> String {
        self.session_id.clone()
    }
}

#[async_trait]
impl AiAgent for CopilotAgent {
    async fn prompt(&self, message: &str) -> anyhow::Result<()> {
        match self.runtime.prompt(&self.session_id, message).await {
            Ok(_) => {
                self.message_count.fetch_add(1, Ordering::SeqCst);
                let _ = self.event_tx.send(AgentEvent::AgentEnd {
                    success: true,
                    error: None,
                });
                Ok(())
            }
            Err(e) => {
                let err = e.to_string();
                let _ = self.event_tx.send(AgentEvent::Error {
                    message: err.clone(),
                });
                let _ = self.event_tx.send(AgentEvent::AgentEnd {
                    success: false,
                    error: Some(err.clone()),
                });
                anyhow::bail!(err);
            }
        }
    }

    async fn set_session_name(&self, _name: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn get_state(&self) -> anyhow::Result<AgentState> {
        let model = self.current_model.read().await.clone();
        Ok(AgentState {
            message_count: self.message_count.load(Ordering::SeqCst),
            model,
        })
    }

    async fn compact(&self) -> anyhow::Result<()> {
        self.runtime.prompt(&self.session_id, "/compact").await?;
        self.message_count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn abort(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn clear(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn set_model(&self, provider: &str, model_id: &str) -> anyhow::Result<()> {
        self.runtime.set_model(&self.session_id, model_id).await?;
        {
            let mut current = self.current_model.write().await;
            *current = Some(model_id.to_string());
        }

        let mut config = crate::commands::agent::ChannelConfig::load().await?;
        if let Some(entry) = config.channels.get_mut(&self.channel_id.to_string()) {
            entry.model_provider = Some(provider.to_string());
            entry.model_id = Some(model_id.to_string());
            if let Err(e) = config.save().await {
                error!("❌ Failed to persist Copilot model selection: {}", e);
            }
        }
        Ok(())
    }

    async fn set_thinking_level(&self, _level: &str) -> anyhow::Result<()> {
        anyhow::bail!("Copilot backend does not support thinking level setting")
    }

    async fn get_available_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        let mut models = self.models.read().await.clone();
        if models.is_empty() {
            if let Some(info) = self.runtime.cached_session_info(&self.session_id).await {
                models = info.models;
                let mut lock = self.models.write().await;
                *lock = models.clone();
            }
        }
        Ok(models)
    }

    async fn load_skill(&self, _name: &str) -> anyhow::Result<()> {
        anyhow::bail!("Copilot backend does not support loading skills")
    }

    fn subscribe_events(&self) -> broadcast::Receiver<AgentEvent> {
        self.event_tx.subscribe()
    }

    fn agent_type(&self) -> &'static str {
        "copilot"
    }
}

#[cfg(test)]
mod tests {
    use super::{CopilotRuntime, SessionUpdateAction};
    use serde_json::json;

    #[test]
    fn test_update_text_and_value_text_extract_text() {
        let update = json!({
            "content": {"text": "abc"}
        });
        assert_eq!(CopilotRuntime::update_text(&update), Some("abc".to_string()));

        let v = json!({"text":"hello"});
        let out = CopilotRuntime::value_text(&v);
        assert!(out.contains("\"text\""));
    }

    #[test]
    fn test_error_text_formats_object_and_string() {
        let err_obj = json!({"message": "boom"});
        assert_eq!(CopilotRuntime::error_text(&err_obj), "boom");
        let err_str = json!("oops");
        assert_eq!(CopilotRuntime::error_text(&err_str), "Unknown error");
    }

    #[test]
    fn test_parse_session_bootstrap_parses_models_and_current_model() {
        let result = json!({
            "sessionId": "sid-1",
            "models": {
                "availableModels": [
                    {"modelId":"m1","name":"M1"},
                    {"modelId":"m2","name":"M2"}
                ],
                "currentModelId": "m2"
            }
        });
        let parsed = CopilotRuntime::parse_session_bootstrap(result).expect("parse");
        assert_eq!(parsed.session_id, "sid-1");
        assert_eq!(parsed.info.models.len(), 2);
        assert_eq!(parsed.info.current_model.as_deref(), Some("m2"));
    }

    #[test]
    fn test_permission_option_id_prefers_allow_always() {
        let msg = json!({
            "params": {
                "options": [
                    {"optionId":"allow_once"},
                    {"optionId":"allow_always_workspace"}
                ]
            }
        });
        assert_eq!(
            CopilotRuntime::permission_option_id(&msg).as_deref(),
            Some("allow_always_workspace")
        );
    }

    #[test]
    fn test_parse_session_update_variants() {
        let thought = json!({"sessionUpdate":"agent_thought_chunk","content":{"text":"hmm"}});
        assert_eq!(
            CopilotRuntime::parse_session_update(&thought),
            SessionUpdateAction::MessageUpdate {
                thinking: "hmm".to_string(),
                text: "".to_string(),
                is_delta: true,
                id: None
            }
        );

        let tool = json!({"sessionUpdate":"tool_call","toolCallId":"t1","status":"running","title":"Shell"});
        assert_eq!(
            CopilotRuntime::parse_session_update(&tool),
            SessionUpdateAction::ToolStart {
                id: "t1".to_string(),
                name: "Shell".to_string()
            }
        );

        let update = json!({"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"done","rawOutput":{"ok":true}});
        let parsed = CopilotRuntime::parse_session_update(&update);
        match parsed {
            SessionUpdateAction::ToolUpdate { id, output } => {
                assert_eq!(id, "t1");
                assert!(output.contains("\"ok\""));
            }
            _ => panic!("expected tool update"),
        }
    }

    #[test]
    fn test_permission_option_id_fallback_and_none() {
        let msg = json!({
            "params": {
                "options": [
                    {"optionId":"allow_once"}
                ]
            }
        });
        assert_eq!(
            CopilotRuntime::permission_option_id(&msg).as_deref(),
            Some("allow_once")
        );

        let empty = json!({"params":{"options":[]}});
        assert!(CopilotRuntime::permission_option_id(&empty).is_none());
    }

    #[test]
    fn test_parse_session_update_ignore_paths() {
        let non_running = json!({"sessionUpdate":"tool_call","toolCallId":"t1","status":"done"});
        assert_eq!(
            CopilotRuntime::parse_session_update(&non_running),
            SessionUpdateAction::Ignore
        );

        let empty_update = json!({"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"","rawOutput":null});
        assert_eq!(
            CopilotRuntime::parse_session_update(&empty_update),
            SessionUpdateAction::Ignore
        );

        let unknown = json!({"sessionUpdate":"other"});
        assert_eq!(
            CopilotRuntime::parse_session_update(&unknown),
            SessionUpdateAction::Ignore
        );
    }

    #[test]
    fn test_parse_session_update_message_chunk() {
        let msg = json!({"sessionUpdate":"agent_message_chunk","text":"hello"});
        assert_eq!(
            CopilotRuntime::parse_session_update(&msg),
            SessionUpdateAction::MessageUpdate {
                thinking: "".to_string(),
                text: "hello".to_string(),
                is_delta: true,
                id: None
            }
        );
    }

    #[test]
    fn test_parse_session_bootstrap_missing_session_id_fails() {
        let result = json!({
            "models": {
                "availableModels": [],
                "currentModelId": null
            }
        });
        let err = CopilotRuntime::parse_session_bootstrap(result).expect_err("should fail");
        assert!(err.to_string().contains("Missing sessionId"));
    }

    #[test]
    fn test_value_text_string_passthrough_and_tool_update_status_fallback() {
        assert_eq!(CopilotRuntime::value_text(&json!("raw")), "raw");

        let update = json!({
            "sessionUpdate":"tool_call_update",
            "toolCallId":"t2",
            "status":"running",
            "rawOutput":null
        });
        assert_eq!(
            CopilotRuntime::parse_session_update(&update),
            SessionUpdateAction::ToolUpdate {
                id: "t2".to_string(),
                output: "running".to_string()
            }
        );
    }

    #[test]
    fn test_permission_option_id_without_options_returns_none() {
        let msg = json!({"params":{}});
        assert!(CopilotRuntime::permission_option_id(&msg).is_none());
    }
}
