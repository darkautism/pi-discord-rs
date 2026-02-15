use super::{AgentEvent, AgentState, AiAgent, ModelInfo};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};
use tokio::sync::{broadcast, Mutex};
use tracing::{info, warn};

pub struct PiAgent {
    stdin: Arc<Mutex<ChildStdin>>,
    event_tx: broadcast::Sender<AgentEvent>,
    _child: tokio::process::Child,
    is_processing: Arc<AtomicBool>,
    session_id: String,
}

impl PiAgent {
    pub async fn new(
        channel_id: u64,
        session_dir: &PathBuf,
    ) -> anyhow::Result<(Arc<Self>, u64)> {
        std::fs::create_dir_all(session_dir)?;

        let pi_binary = std::env::var("PI_BINARY").unwrap_or_else(|_| "pi".to_string());
        let mut cmd = Command::new(pi_binary);
        cmd.arg("--mode").arg("rpc");

        let session_file = session_dir.join(format!("discord-rs-{}.jsonl", channel_id));
        cmd.arg("--session").arg(&session_file);
        cmd.arg("--session-dir").arg(session_dir);

        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        info!("🚀 Started pi process for channel {}: {:?}", channel_id, cmd);

        let stdin_raw = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to open stdin"))?;
        let stdin = Arc::new(Mutex::new(stdin_raw));
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to open stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to open stderr"))?;

        let (event_tx, _) = broadcast::channel(1000);
        let tx = event_tx.clone();

        // Task to log stderr
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            while let Ok(n) = reader.read_line(&mut line).await {
                if n == 0 {
                    break;
                }
                info!("[PI-STDERR-{}]: {}", channel_id, line.trim());
                line.clear();
            }
        });

        // Task to parse stdout
        let tx_c = tx.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            while let Ok(n) = reader.read_line(&mut line).await {
                if n == 0 {
                    info!("🔌 Pi process stdout closed for channel {}", channel_id);
                    let _ = tx_c.send(AgentEvent::ConnectionError {
                        message: "Pi process exited unexpectedly.".to_string(),
                    });
                    break;
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
                    Self::parse_event(&tx_c, val);
                } else {
                    info!("[PI-STDOUT-{}]: {}", channel_id, trimmed);
                }
                line.clear();
            }
        });

        let agent = Arc::new(PiAgent {
            stdin,
            event_tx,
            _child: child,
            is_processing: Arc::new(AtomicBool::new(false)),
            session_id: format!("pi-{}", channel_id),
        });

        // Initial setup - just send the commands without waiting for response
        // Pi RPC will process them in order
        agent
            .raw_call(json!({
                "type": "set_session_name",
                "name": format!("discord-rs-{}", channel_id)
            }))
            .await?;

        // Give Pi a moment to process the initial setup
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        Ok((agent, 0))
    }

    fn parse_event(tx: &broadcast::Sender<AgentEvent>, val: Value) {
        match val["type"].as_str() {
            Some("message_update") | Some("text_delta") | Some("thinking_delta") | Some("text_end") | Some("message_end") => {
                let delta_obj = if val.get("assistantMessageEvent").is_some() {
                    &val["assistantMessageEvent"]
                } else if val.get("message").is_some() {
                    &val["message"]
                } else {
                    &val
                };

                let mut thinking = String::new();
                let mut text = String::new();
                let mut is_delta = true;

                let content_val = delta_obj.get("partial").and_then(|p| p.get("content"))
                    .or_else(|| delta_obj.get("content").filter(|c| c.is_array()));

                if let Some(content) = content_val.and_then(|c| c.as_array()) {
                    is_delta = false;
                    for item in content {
                        match item["type"].as_str() {
                            Some("thinking") => {
                                thinking.push_str(item["thinking"].as_str().unwrap_or(""))
                            }
                            Some("text") => text.push_str(item["text"].as_str().unwrap_or("")),
                            _ => {}
                        }
                    }
                } else {
                    if let Some(c) = delta_obj.get("content").and_then(|c| c.as_str()) {
                        text = c.to_string();
                        is_delta = false;
                    } else if let Some(d) = delta_obj.get("delta").and_then(|d| d.as_str()) {
                        let t = delta_obj["type"].as_str().unwrap_or("");
                        if t == "thinking_delta" || t == "thinking" {
                            thinking.push_str(d);
                        } else if t == "text_delta" || t == "text" {
                            text.push_str(d);
                        }
                    } else {
                        let t = delta_obj["type"].as_str().unwrap_or("");
                        if t == "thinking_delta" {
                            thinking.push_str(delta_obj["delta"].as_str().unwrap_or(""));
                        } else if t == "text_delta" {
                            text.push_str(delta_obj["delta"].as_str().unwrap_or(""));
                        }
                    }
                }

                if !thinking.is_empty() || !text.is_empty() {
                    let _ = tx.send(AgentEvent::MessageUpdate { thinking, text, is_delta });
                }

                if delta_obj["type"] == "error" {
                    let err_msg = delta_obj["errorMessage"].as_str().unwrap_or("Unknown API error");
                    if delta_obj["reason"] == "aborted" {
                        let _ = tx.send(AgentEvent::AgentEnd { success: false, error: Some("Aborted".to_string()) });
                    } else {
                        let _ = tx.send(AgentEvent::Error { message: err_msg.to_string() });
                    }
                }
            }
            Some("tool_execution_start") => {
                let name = val["toolName"].as_str().unwrap_or("tool").to_string();
                let _ = tx.send(AgentEvent::ToolExecutionStart { name });
            }
            Some("tool_execution_update") => {
                if let Some(content) = val.get("partialResult").and_then(|p| p.get("content")).and_then(|c| c.as_array()) {
                    let mut output = String::new();
                    for item in content {
                        if let Some(text) = item["text"].as_str() {
                            output.push_str(text);
                        }
                    }
                    if !output.is_empty() {
                        let _ = tx.send(AgentEvent::ToolExecutionUpdate { output });
                    }
                }
            }
            Some("tool_execution_end") => {
                let _ = tx.send(AgentEvent::ToolExecutionEnd { name: "tool".to_string() });
            }
            Some("turn_end") => {
                // Ignore turn_end as it occurs after tools but before final message
            }
            Some("agent_end") => {
                if let Some(err) = val["errorMessage"].as_str() {
                    let _ = tx.send(AgentEvent::AgentEnd {
                        success: false,
                        error: Some(err.to_string()),
                    });
                } else {
                    let _ = tx.send(AgentEvent::AgentEnd {
                        success: true,
                        error: None,
                    });
                }
            }
            Some("response") => {
                if let Some(id) = val["id"].as_str() {
                    let _ = tx.send(AgentEvent::CommandResponse { id: id.to_string(), data: val["data"].clone() });
                }
            }
            Some("error") => {
                let err_msg = val["message"].as_str().or(val["error"].as_str()).unwrap_or("Unknown top-level error");
                let _ = tx.send(AgentEvent::Error { message: err_msg.to_string() });
            }
            _ => {}
        }
    }

    async fn raw_call(&self, mut cmd: Value) -> anyhow::Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        if let Some(obj) = cmd.as_object_mut() {
            obj.insert("id".to_string(), json!(id));
        } else {
            anyhow::bail!("Command is not a JSON object");
        }
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all((serde_json::to_string(&cmd)? + "\n").as_bytes())
            .await?;
        stdin.flush().await?;
        Ok(id)
    }

    pub fn is_processing(&self) -> bool {
        self.is_processing.load(Ordering::SeqCst)
    }

    pub fn set_processing(&self, value: bool) {
        self.is_processing.store(value, Ordering::SeqCst);
    }
}

#[async_trait]
impl AiAgent for PiAgent {
    async fn prompt(&self, message: &str) -> anyhow::Result<()> {
        self.raw_call(json!({
            "type": "prompt",
            "message": message,
            "stream": true,
            "streamingBehavior": "steer"
        }))
        .await?;
        Ok(())
    }

    async fn set_session_name(&self, name: &str) -> anyhow::Result<()> {
        self.raw_call(json!({ "type": "set_session_name", "name": name }))
            .await?;
        Ok(())
    }

    async fn get_state(&self) -> anyhow::Result<AgentState> {
        if let Some(channel_id_str) = self.session_id.strip_prefix("pi-") {
            let session_dir = crate::migrate::get_sessions_dir("pi");
            let session_file = session_dir.join(format!("discord-rs-{}.jsonl", channel_id_str));
            
            if session_file.exists() {
                let content = tokio::fs::read_to_string(&session_file).await.unwrap_or_default();
                let count = content.lines().count() as u64;
                return Ok(AgentState {
                    message_count: count,
                    model: None,
                });
            }
        }

        Ok(AgentState {
            message_count: 0,
            model: None,
        })
    }

    async fn compact(&self) -> anyhow::Result<()> {
        self.raw_call(json!({ "type": "compact" })).await?;
        Ok(())
    }

    async fn abort(&self) -> anyhow::Result<()> {
        self.raw_call(json!({ "type": "abort" })).await?;
        Ok(())
    }

    async fn clear(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn set_model(&self, provider: &str, model_id: &str) -> anyhow::Result<()> {
        self.raw_call(json!({
            "type": "set_model",
            "provider": provider,
            "modelId": model_id
        }))
        .await?;
        Ok(())
    }

    async fn set_thinking_level(&self, level: &str) -> anyhow::Result<()> {
        self.raw_call(json!({ "type": "set_thinking_level", "level": level }))
            .await?;
        Ok(())
    }

    async fn get_available_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        let cmd_id = self.raw_call(json!({ "type": "get_available_models" })).await?;
        let mut rx = self.subscribe_events();
        
        let result = tokio::time::timeout(
            tokio::time::Duration::from_secs(5),
            async {
                loop {
                    match rx.recv().await {
                        Ok(AgentEvent::CommandResponse { id, data }) => {
                            if id == cmd_id {
                                if let Some(models) = data["models"].as_array() {
                                    return Ok(models
                                        .iter()
                                        .take(25)
                                        .filter_map(|m| {
                                            let provider = m["provider"].as_str()?;
                                            let model_id = m["id"].as_str()?;
                                            Some(ModelInfo {
                                                provider: provider.to_string(),
                                                id: model_id.to_string(),
                                                label: format!("{}/{}", provider, model_id),
                                            })
                                        })
                                        .collect());
                                }
                                return Ok(vec![]);
                            }
                        }
                        Ok(AgentEvent::Error { message }) => {
                            return Err(anyhow::anyhow!("Agent error: {}", message));
                        }
                        Err(_) => {
                            return Err(anyhow::anyhow!("Event channel closed"));
                        }
                        _ => continue,
                    }
                }
            }
        ).await;
        
        match result {
            Ok(models) => models,
            Err(_) => Err(anyhow::anyhow!("Timeout waiting for model list from Pi")),
        }
    }

    async fn load_skill(&self, name: &str) -> anyhow::Result<()> {
        self.raw_call(json!({ "type": "load_skill", "name": name }))
            .await?;
        Ok(())
    }

    fn subscribe_events(&self) -> broadcast::Receiver<AgentEvent> {
        self.event_tx.subscribe()
    }

    fn agent_type(&self) -> &'static str {
        "pi"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_parse_event_message_update_delta() {
        let (tx, mut rx) = broadcast::channel(10);
        let val = json!({
            "type": "message_update",
            "assistantMessageEvent": {
                "type": "text_delta",
                "delta": "hello"
            }
        });

        PiAgent::parse_event(&tx, val);
        let event = rx.recv().await.unwrap();
        if let AgentEvent::MessageUpdate { text, is_delta, .. } = event {
            assert_eq!(text, "hello");
            assert!(is_delta);
        } else {
            panic!("Expected MessageUpdate");
        }
    }

    #[tokio::test]
    async fn test_parse_event_message_update_partial() {
        let (tx, mut rx) = broadcast::channel(10);
        let val = json!({
            "type": "message_update",
            "assistantMessageEvent": {
                "type": "text_end",
                "partial": {
                    "content": [
                        {"type": "thinking", "thinking": "reasoning"},
                        {"type": "text", "text": "final answer"}
                    ]
                }
            }
        });

        PiAgent::parse_event(&tx, val);
        let event = rx.recv().await.unwrap();
        if let AgentEvent::MessageUpdate { thinking, text, is_delta } = event {
            assert_eq!(thinking, "reasoning");
            assert_eq!(text, "final answer");
            assert!(!is_delta);
        } else {
            panic!("Expected MessageUpdate");
        }
    }

    #[tokio::test]
    async fn test_parse_event_root_delta() {
        let (tx, mut rx) = broadcast::channel(10);
        let val = json!({
            "type": "text_delta",
            "delta": "world"
        });

        PiAgent::parse_event(&tx, val);
        let event = rx.recv().await.unwrap();
        if let AgentEvent::MessageUpdate { text, is_delta, .. } = event {
            assert_eq!(text, "world");
            assert!(is_delta);
        } else {
            panic!("Expected MessageUpdate");
        }
    }

    #[tokio::test]
    async fn test_parse_event_agent_end_with_messages() {
        let (tx, mut rx) = broadcast::channel(10);
        let val = json!({
            "type": "agent_end",
            "messages": [
                {
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "captured from end"}
                    ]
                }
            ]
        });

        PiAgent::parse_event(&tx, val);
        
        // Should get MessageUpdate first
        let event1 = rx.recv().await.unwrap();
        if let AgentEvent::MessageUpdate { text, is_delta, .. } = event1 {
            assert_eq!(text, "captured from end");
            assert!(!is_delta);
        } else {
            panic!("Expected MessageUpdate");
        }
    }

    #[tokio::test]
    async fn test_parse_event_complex_flow_with_tools() {
        let (tx, mut rx) = broadcast::channel(20);
        
        // 1. Initial message start
        PiAgent::parse_event(&tx, json!({"type": "message_start", "message": {"role": "assistant"}}));
        
        // 2. Thinking delta
        PiAgent::parse_event(&tx, json!({
            "type": "thinking_delta",
            "delta": "Checking system status..."
        }));
        let ev = rx.recv().await.unwrap();
        if let AgentEvent::MessageUpdate { thinking, is_delta, .. } = ev {
            assert_eq!(thinking, "Checking system status...");
            assert!(is_delta);
        }

        // 3. Tool start
        PiAgent::parse_event(&tx, json!({
            "type": "tool_execution_start",
            "toolName": "bash"
        }));
        let ev = rx.recv().await.unwrap();
        assert!(matches!(ev, AgentEvent::ToolExecutionStart { .. }));

        // 4. Tool update (Long output that should be truncated)
        let long_output = "line1\n".repeat(100); // 600 chars
        PiAgent::parse_event(&tx, json!({
            "type": "tool_execution_update",
            "partialResult": {
                "content": [{"type": "text", "text": long_output}]
            }
        }));
        let ev = rx.recv().await.unwrap();
        if let AgentEvent::MessageUpdate { text, .. } = ev {
            assert!(text.contains("```"));
            assert!(text.len() <= 300); // Should be truncated to last 200 chars + markdown overhead
        }

        // 5. Turn end (The "Stupid Problem": this should NOT trigger AgentEnd)
        PiAgent::parse_event(&tx, json!({"type": "turn_end"}));
        // Verify no AgentEnd was sent
        assert!(rx.try_recv().is_err());

        // 6. Final summary message
        PiAgent::parse_event(&tx, json!({
            "type": "text_delta",
            "delta": "All systems green."
        }));
        let ev = rx.recv().await.unwrap();
        if let AgentEvent::MessageUpdate { text, .. } = ev {
            assert_eq!(text, "All systems green.");
        }

        // 7. Actual Agent end
        PiAgent::parse_event(&tx, json!({"type": "agent_end"}));
        let ev = rx.recv().await.unwrap();
        assert!(matches!(ev, AgentEvent::AgentEnd { success: true, .. }));
    }
}
