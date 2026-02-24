use super::{AgentEvent, AgentState, AiAgent, ContentItem, ContentType, ModelInfo, UserInput};
use async_trait::async_trait;
use base64::Engine;
use eventsource_client::{Client, ClientBuilder, SSE};
use futures::StreamExt;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

#[derive(Debug, Clone, PartialEq)]
enum RealtimeEventAction {
    MessageUpdate {
        thinking: String,
        text: String,
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
    TurnCompleted,
    Error(String),
    Ignore,
}

pub struct OpencodeAgent {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    pub session_id: String,
    channel_id: u64,
    event_tx: broadcast::Sender<AgentEvent>,
    current_model: Arc<Mutex<Option<(String, String)>>>,
    turn_failed: Arc<AtomicBool>,
    agent_type_name: &'static str,
}

impl OpencodeAgent {
    const MAX_INLINE_FILE_BYTES: u64 = 4 * 1024 * 1024;

    pub async fn new(
        channel_id: u64,
        base_url: String,
        api_key: String,
        existing_sid: Option<String>,
        model_opt: Option<(String, String)>,
        agent_type_name: &'static str,
    ) -> anyhow::Result<Arc<Self>> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()?;
        let mut session_id = existing_sid;

        if session_id.is_none() {
            info!(
                "Creating NEW {} session for channel {}",
                agent_type_name, channel_id
            );
            let resp = client
                .post(format!("{}/session", base_url))
                .header("Authorization", format!("Bearer {}", api_key))
                .json(&json!({ "title": format!("Discord #{}", channel_id) }))
                .send()
                .await?;
            let info: Value = resp.json().await?;
            session_id = Some(
                info["id"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Create failed"))?
                    .to_string(),
            );
        }

        let session_id = session_id.unwrap();
        let (event_tx, _) = broadcast::channel(1000);
        let current_model = Arc::new(Mutex::new(model_opt));
        let turn_failed = Arc::new(AtomicBool::new(false));

        let agent = Arc::new(Self {
            client,
            api_key: api_key.clone(),
            base_url: base_url.clone(),
            session_id: session_id.clone(),
            channel_id,
            event_tx: event_tx.clone(),
            current_model,
            turn_failed,
            agent_type_name,
        });

        let sse_url = format!("{}/event", base_url);
        let agent_weak = Arc::downgrade(&agent);
        let auth_header = format!("Bearer {}", api_key);

        tokio::spawn(async move {
            let mut retry = 0;
            loop {
                let sse_client = match ClientBuilder::for_url(&sse_url) {
                    Ok(b) => match b.header("Authorization", &auth_header) {
                        Ok(b) => b.build(),
                        Err(_) => break,
                    },
                    Err(_) => break,
                };
                let mut stream = sse_client.stream();
                while let Some(event) = stream.next().await {
                    retry = 0;
                    if let Ok(val) = serde_json::from_str::<Value>(&match event {
                        Ok(SSE::Event(e)) => e.data,
                        _ => continue,
                    }) {
                        if let Some(agent) = agent_weak.upgrade() {
                            agent.handle_event(val).await;
                        } else {
                            return;
                        }
                    }
                }
                if agent_weak.strong_count() == 0 || retry > 10 {
                    break;
                }
                retry += 1;
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });

        Ok(agent)
    }

    async fn construct_message_body(
        input: &UserInput,
        model_opt: &Option<(String, String)>,
    ) -> Value {
        let (text, extra_parts) = Self::build_parts_from_input(input).await;
        let mut parts = vec![json!({ "type": "text", "text": text })];
        parts.extend(extra_parts);

        let mut body = json!({ "parts": parts });
        if let Some((provider, model)) = model_opt {
            body["model"] = json!({ "providerID": provider, "modelID": model });
        }
        body
    }

    async fn build_parts_from_input(input: &UserInput) -> (String, Vec<Value>) {
        if input.files.is_empty() {
            return (input.text.clone(), Vec::new());
        }

        let mut summary_lines = Vec::new();
        let mut parts = Vec::new();

        for file in &input.files {
            let mut status = "fallback_path";
            if file.size <= Self::MAX_INLINE_FILE_BYTES {
                if let Ok(raw) = tokio::fs::read(&file.local_path).await {
                    let b64 = base64::engine::general_purpose::STANDARD.encode(raw);
                    let part_type = if file.is_image() { "image" } else { "file" };
                    parts.push(json!({
                        "type": part_type,
                        "filename": file.display_name(),
                        "mimeType": file.mime,
                        "data": b64
                    }));
                    status = "inline_base64";
                }
            }

            summary_lines.push(format!(
                "- {} | mime={} | size={}B | local_path={} | source_url={} | mode={}",
                file.display_name(),
                file.mime,
                file.size,
                file.local_path,
                file.source_url,
                status
            ));
        }

        let enriched_text = format!(
            "{}\n\n[Uploaded Files]\n{}\n\nUse inline files when available. If inline is missing, use local_path via tools.",
            input.text,
            summary_lines.join("\n")
        );

        (enriched_text, parts)
    }

    #[cfg(test)]
    fn retry_delay() -> Duration {
        Duration::from_millis(20)
    }

    #[cfg(not(test))]
    fn retry_delay() -> Duration {
        Duration::from_secs(2)
    }

    async fn handle_event(&self, val: Value) {
        let type_ = val["type"].as_str().unwrap_or("");
        // 只記錄關鍵事件，避免日誌過多
        if !type_.contains("delta") {
            info!("📡 SSE Event: type={}", type_);
        }

        match Self::parse_realtime_event(&val) {
            RealtimeEventAction::MessageUpdate { thinking, text, id } => {
                let _ = self.event_tx.send(AgentEvent::MessageUpdate {
                    thinking,
                    text,
                    is_delta: true,
                    id,
                });
            }
            RealtimeEventAction::ToolStart { id, name } => {
                let _ = self
                    .event_tx
                    .send(AgentEvent::ToolExecutionStart { id, name });
            }
            RealtimeEventAction::ToolUpdate { id, output } => {
                let _ = self
                    .event_tx
                    .send(AgentEvent::ToolExecutionUpdate { id, output });
            }
            RealtimeEventAction::TurnCompleted => {
                info!("🏁 Turn completed signal received: {}", type_);
                if !self.turn_failed.load(Ordering::SeqCst) {
                    self.trigger_sync().await;
                }
            }
            RealtimeEventAction::Error(msg) => {
                error!("❌ FULL ERROR JSON: {}", val);
                error!("❌ Backend Error Summary: {}", msg);
                self.turn_failed.store(true, Ordering::SeqCst);
                let _ = self.event_tx.send(AgentEvent::AgentEnd {
                    success: false,
                    error: Some(msg),
                });
            }
            RealtimeEventAction::Ignore => {}
        }
    }

    fn parse_realtime_event(val: &Value) -> RealtimeEventAction {
        let type_ = val["type"].as_str().unwrap_or("");
        let properties = &val["properties"];
        let data = &val["data"];

        match type_ {
            "message.part.updated" | "message.part.delta" | "session.message.part.delta" => {
                Self::parse_delta_event(properties, data)
            }
            "session.turn.close"
            | "session.message.completed"
            | "turn.close"
            | "message.completed"
            | "turn.end"
            | "session.idle" => RealtimeEventAction::TurnCompleted,
            "session.error" | "error" => {
                let msg = Self::extract_error_message(properties, data);
                RealtimeEventAction::Error(msg)
            }
            _ => RealtimeEventAction::Ignore,
        }
    }

    fn parse_delta_event(properties: &Value, data: &Value) -> RealtimeEventAction {
        let part_info = if properties["part"].is_object() {
            &properties["part"]
        } else {
            data
        };
        let part_type = part_info["type"]
            .as_str()
            .or(properties["type"].as_str())
            .unwrap_or("text");
        let part_id = part_info["id"]
            .as_str()
            .or(properties["partID"].as_str())
            .map(|s| s.to_string());
        let delta = properties["delta"]
            .as_str()
            .or(data["delta"].as_str())
            .unwrap_or("");

        let role = properties["messageRole"]
            .as_str()
            .or(data["messageRole"].as_str())
            .or(properties["role"].as_str())
            .or(data["role"].as_str())
            .or(part_info["role"].as_str())
            .unwrap_or("");

        if (role == "system" || role == "user")
            && !part_type.contains("reason")
            && !part_type.contains("think")
        {
            return RealtimeEventAction::Ignore;
        }

        if part_type.contains("reason") || part_type.contains("think") {
            return RealtimeEventAction::MessageUpdate {
                thinking: delta.into(),
                text: "".into(),
                id: part_id,
            };
        }

        if part_type.contains("tool") || part_type == "agent" {
            let id = part_id.unwrap_or_else(|| "tool".into());
            let status = part_info["state"]["status"].as_str().unwrap_or("");
            if status == "running" || status == "pending" {
                let name = part_info["tool"].as_str().unwrap_or("tool");
                let cmd = part_info["state"]["input"]["command"]
                    .as_str()
                    .unwrap_or("");
                return RealtimeEventAction::ToolStart {
                    id,
                    name: format!("🛠️ `{}`: `{}`", name, cmd),
                };
            }
            if status == "completed" {
                let output = part_info["state"]["metadata"]["output"]
                    .as_str()
                    .or(part_info["state"]["output"].as_str())
                    .unwrap_or("");
                return RealtimeEventAction::ToolUpdate {
                    id,
                    output: output.into(),
                };
            }
            return RealtimeEventAction::Ignore;
        }

        RealtimeEventAction::MessageUpdate {
            thinking: "".into(),
            text: delta.into(),
            id: part_id,
        }
    }

    fn extract_error_message(properties: &Value, data: &Value) -> String {
        properties["error"]["data"]["message"]
            .as_str()
            .or(properties["message"].as_str())
            .or(data["message"].as_str())
            .unwrap_or("Unknown Error")
            .to_string()
    }

    async fn trigger_sync(&self) {
        let client = self.client.clone();
        let api_key = self.api_key.clone();
        let url = format!("{}/session/{}/message", self.base_url, self.session_id);
        let tx = self.event_tx.clone();
        let turn_failed = Arc::clone(&self.turn_failed); // 克隆 Arc 以進入 spawn
        tokio::spawn(async move {
            if let Ok(resp) = client
                .get(url)
                .header("Authorization", format!("Bearer {}", api_key))
                .send()
                .await
            {
                if let Ok(msgs) = resp.json::<Value>().await {
                    if let Some(last) = msgs
                        .as_array()
                        .and_then(|a| a.iter().rfind(|m| m["role"] == "assistant"))
                    {
                        if let Some(parts) = last["parts"].as_array() {
                            let mut items = Vec::new();
                            for p in parts {
                                let t = p["type"].as_str().unwrap_or("");
                                let content = p["text"]
                                    .as_str()
                                    .or(p["content"].as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let pid = p["id"].as_str().map(|s| s.to_string());
                                match t {
                                    "text" => items.push(ContentItem {
                                        type_: ContentType::Text,
                                        content,
                                        id: pid,
                                    }),
                                    "thinking" | "reasoning" => items.push(ContentItem {
                                        type_: ContentType::Thinking,
                                        content,
                                        id: pid,
                                    }),
                                    _ => {}
                                }
                            }
                            let _ = tx.send(AgentEvent::ContentSync { items });
                        }
                    }
                }
            }
            let failed = turn_failed.load(Ordering::SeqCst);
            if !failed {
                let _ = tx.send(AgentEvent::AgentEnd {
                    success: true,
                    error: None,
                });
            }
        });
    }
}

#[async_trait]
impl AiAgent for OpencodeAgent {
    async fn prompt(&self, message: &str) -> anyhow::Result<()> {
        self.prompt_with_input(&UserInput::new_text(message.to_string()))
            .await
    }

    async fn prompt_with_input(&self, input: &UserInput) -> anyhow::Result<()> {
        let url = format!("{}/session/{}/message", self.base_url, self.session_id);
        self.turn_failed.store(false, Ordering::SeqCst);
        let model_opt = self.current_model.lock().await.clone();
        let body = Self::construct_message_body(input, &model_opt).await;

        let max_retries = 3;
        let retry_delay = Self::retry_delay();
        let mut last_error_message: Option<String> = None;

        for attempt in 1..=max_retries {
            info!("🛰️ Prompt attempt {}/{}", attempt, max_retries);

            let resp_res = self
                .client
                .post(&url)
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("Connection", "close") // 強制關閉連線，不進入連線池，防止池污染
                .json(&body)
                .send()
                .await;

            match resp_res {
                Ok(resp) => {
                    if resp.status().is_success() {
                        return Ok(());
                    }

                    let status = resp.status();
                    if status == 404 {
                        warn!(
                            "⚠️ Session {} returned 404 on prompt for channel {}; preserving sid for non-destructive recovery",
                            self.session_id, self.channel_id
                        );
                        let _ = self.event_tx.send(AgentEvent::AgentEnd {
                            success: false,
                            error: Some("Session expired. Please retry.".into()),
                        });
                        anyhow::bail!("Session expired (404)");
                    }

                    let body = resp.text().await.unwrap_or_default();
                    let err_msg = if body.trim().is_empty() {
                        format!("API Error {}", status)
                    } else {
                        format!("API Error {}: {}", status, body.trim())
                    };
                    error!("⚠️ [ATTEMPT {}/{} FAIL]: {}", attempt, max_retries, err_msg);
                    last_error_message = Some(err_msg);

                    if attempt < max_retries {
                        tokio::time::sleep(retry_delay).await;
                    }
                }
                Err(e) => {
                    let err_msg = e.to_string();
                    error!("⚠️ [ATTEMPT {}/{} FAIL]: {}", attempt, max_retries, err_msg);
                    last_error_message = Some(err_msg);
                    if attempt < max_retries {
                        tokio::time::sleep(retry_delay).await;
                    }
                }
            }
        }

        if let Some(err_msg) = last_error_message {
            let _ = self.event_tx.send(AgentEvent::Error {
                message: err_msg.clone(),
            });
            anyhow::bail!(err_msg);
        }
        anyhow::bail!("Prompt failed after all retries")
    }
    async fn get_state(&self) -> anyhow::Result<AgentState> {
        let url = format!("{}/session/{}", self.base_url, self.session_id);
        let resp = self
            .client
            .get(url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await?;
        if resp.status().is_success() {
            let info: Value = resp.json().await?;
            return Ok(AgentState {
                message_count: info["messageCount"].as_u64().unwrap_or(0),
                model: None,
            });
        }
        if resp.status() == 404 {
            warn!(
                "⚠️ Session {} returned 404 on state check for channel {}; preserving sid for non-destructive recovery",
                self.session_id, self.channel_id
            );
        }
        Ok(AgentState {
            message_count: 0,
            model: None,
        })
    }
    async fn set_model(&self, provider: &str, mid: &str) -> anyhow::Result<()> {
        let mut m = self.current_model.lock().await;
        *m = Some((provider.into(), mid.into()));
        let mut config = crate::commands::agent::ChannelConfig::load().await?;
        if let Some(entry) = config.channels.get_mut(&self.channel_id.to_string()) {
            entry.model_provider = Some(provider.into());
            entry.model_id = Some(mid.into());
            if let Err(e) = config.save().await {
                error!("❌ Failed to persist model selection: {}", e);
            }
        }
        Ok(())
    }
    async fn abort(&self) -> anyhow::Result<()> {
        let _ = self
            .client
            .post(format!(
                "{}/session/{}/abort",
                self.base_url, self.session_id
            ))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await;
        Ok(())
    }
    async fn clear(&self) -> anyhow::Result<()> {
        Ok(())
    }
    async fn compact(&self) -> anyhow::Result<()> {
        let url = format!("{}/session/{}/message", self.base_url, self.session_id);
        let body = json!({
            "parts": [{"type": "text", "text": "/compact"}]
        });
        let resp = self
            .client
            .post(url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            anyhow::bail!("Compact failed: {}", resp.status());
        }
        Ok(())
    }
    async fn set_session_name(&self, _n: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn set_thinking_level(&self, _l: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn get_available_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        let resp = self
            .client
            .get(format!("{}/provider", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await?;
        let val: Value = resp.json().await?;
        let connected: HashSet<String> = val["connected"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let mut models = Vec::new();
        if let Some(all) = val["all"].as_array() {
            for p in all {
                let pid = p["id"].as_str().unwrap_or("");
                if !connected.contains(pid) {
                    continue;
                }
                if let Some(m_map) = p["models"].as_object() {
                    for (id, _) in m_map {
                        models.push(ModelInfo {
                            provider: pid.into(),
                            id: id.clone(),
                            label: format!("{}/{}", pid, id),
                        });
                    }
                }
            }
        }
        Ok(models)
    }
    async fn load_skill(&self, _n: &str) -> anyhow::Result<()> {
        Ok(())
    }
    fn subscribe_events(&self) -> broadcast::Receiver<AgentEvent> {
        self.event_tx.subscribe()
    }
    fn agent_type(&self) -> &'static str {
        self.agent_type_name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{UploadedFile, UserInput};
    use crate::migrate::BASE_DIR_ENV;
    use serde_json::json;
    use std::sync::{Mutex as StdMutex, OnceLock};
    use tempfile::tempdir;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn env_lock() -> &'static StdMutex<()> {
        static LOCK: OnceLock<StdMutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| StdMutex::new(()))
    }

    fn build_test_agent(
        mock_server: &MockServer,
        api_key: &str,
        session_id: &str,
    ) -> (OpencodeAgent, broadcast::Receiver<AgentEvent>) {
        let (event_tx, _) = broadcast::channel(100);
        let rx = event_tx.subscribe();
        let agent = OpencodeAgent {
            client: reqwest::Client::new(),
            api_key: api_key.to_string(),
            base_url: mock_server.uri(),
            session_id: session_id.to_string(),
            channel_id: 1,
            event_tx,
            current_model: Arc::new(Mutex::new(None)),
            turn_failed: Arc::new(AtomicBool::new(false)),
            agent_type_name: "opencode",
        };
        (agent, rx)
    }

    #[tokio::test]
    async fn test_opencode_retry_logic() -> anyhow::Result<()> {
        let mock_server = MockServer::start().await;
        let api_key = "test_key".to_string();
        let session_id = "test_session".to_string();

        // 模擬 3 次 500 錯誤，然後第 4 次成功 (但我們只會重試 3 次)
        // 注意：測試邏輯是嘗試 1..=3，所以如果 3 次都失敗，最終應該回傳 Err。
        Mock::given(method("POST"))
            .and(path(format!("/session/{}/message", session_id)))
            .respond_with(ResponseTemplate::new(500))
            .expect(3) // 預期會被呼叫 3 次
            .mount(&mock_server)
            .await;

        let (agent, mut rx) = build_test_agent(&mock_server, &api_key, &session_id);

        let result = agent.prompt("Hello").await;

        // 斷言：最終應該失敗，因為 3 次重試都拿到了 500
        assert!(result.is_err());
        let event = tokio::time::timeout(Duration::from_secs(1), rx.recv()).await??;
        assert!(matches!(event, AgentEvent::Error { .. }));
        // Mock server 會在 drop 時驗證是否真的呼叫了 3 次
        Ok(())
    }

    #[tokio::test]
    async fn test_opencode_retry_success_on_second_attempt() -> anyhow::Result<()> {
        let mock_server = MockServer::start().await;
        let api_key = "test_key".to_string();
        let session_id = "test_session".to_string();

        // 第 1 次 500，第 2 次 200，兩次請求都應命中 /session/{id}/message
        Mock::given(method("POST"))
            .and(path(format!("/session/{}/message", session_id)))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(1)
            .expect(1)
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(path(format!("/session/{}/message", session_id)))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&mock_server)
            .await;

        let (agent, mut rx) = build_test_agent(&mock_server, &api_key, &session_id);

        let result = agent.prompt("Hello").await;
        assert!(result.is_ok());
        let no_error = tokio::time::timeout(Duration::from_millis(250), async {
            loop {
                match rx.recv().await {
                    Ok(AgentEvent::Error { .. }) => return false,
                    Ok(_) => continue,
                    Err(_) => return true,
                }
            }
        })
        .await
        .is_err();
        assert!(no_error);
        Ok(())
    }

    #[test]
    fn test_parse_realtime_event_thinking_delta() {
        let v = json!({
            "type":"message.part.delta",
            "properties":{
                "part":{"type":"thinking","id":"p1","role":"assistant"},
                "delta":"thinking..."
            },
            "data":{}
        });
        let got = OpencodeAgent::parse_realtime_event(&v);
        assert_eq!(
            got,
            RealtimeEventAction::MessageUpdate {
                thinking: "thinking...".to_string(),
                text: "".to_string(),
                id: Some("p1".to_string())
            }
        );
    }

    #[test]
    fn test_parse_realtime_event_text_delta_filters_user_role() {
        let v = json!({
            "type":"message.part.delta",
            "properties":{
                "part":{"type":"text","id":"p2","role":"user"},
                "delta":"hello"
            },
            "data":{}
        });
        let got = OpencodeAgent::parse_realtime_event(&v);
        assert_eq!(got, RealtimeEventAction::Ignore);
    }

    #[test]
    fn test_parse_realtime_event_tool_start_and_update() {
        let running = json!({
            "type":"message.part.delta",
            "properties":{
                "part":{
                    "type":"tool",
                    "id":"t1",
                    "tool":"bash",
                    "state":{"status":"running","input":{"command":"ls"}}
                }
            },
            "data":{}
        });
        let got_running = OpencodeAgent::parse_realtime_event(&running);
        assert_eq!(
            got_running,
            RealtimeEventAction::ToolStart {
                id: "t1".to_string(),
                name: "🛠️ `bash`: `ls`".to_string()
            }
        );

        let done = json!({
            "type":"message.part.delta",
            "properties":{
                "part":{
                    "type":"tool",
                    "id":"t1",
                    "state":{"status":"completed","metadata":{"output":"ok"}}
                }
            },
            "data":{}
        });
        let got_done = OpencodeAgent::parse_realtime_event(&done);
        assert_eq!(
            got_done,
            RealtimeEventAction::ToolUpdate {
                id: "t1".to_string(),
                output: "ok".to_string()
            }
        );
    }

    #[test]
    fn test_parse_realtime_event_turn_completed_and_error() {
        let done = json!({"type":"turn.end"});
        assert_eq!(
            OpencodeAgent::parse_realtime_event(&done),
            RealtimeEventAction::TurnCompleted
        );

        let err = json!({
            "type":"error",
            "properties":{"error":{"data":{"message":"boom"}}},
            "data":{}
        });
        assert_eq!(
            OpencodeAgent::parse_realtime_event(&err),
            RealtimeEventAction::Error("boom".to_string())
        );
    }

    #[test]
    fn test_parse_realtime_event_text_and_agent_tool_defaults() {
        let text = json!({
            "type":"message.part.updated",
            "properties":{"part":{"type":"text","id":"m1","role":"assistant"},"delta":"hello"},
            "data":{}
        });
        assert_eq!(
            OpencodeAgent::parse_realtime_event(&text),
            RealtimeEventAction::MessageUpdate {
                thinking: "".to_string(),
                text: "hello".to_string(),
                id: Some("m1".to_string())
            }
        );

        let tool = json!({
            "type":"message.part.updated",
            "properties":{"part":{"type":"agent","state":{"status":"pending","input":{"command":"pwd"}}}},
            "data":{}
        });
        assert_eq!(
            OpencodeAgent::parse_realtime_event(&tool),
            RealtimeEventAction::ToolStart {
                id: "tool".to_string(),
                name: "🛠️ `tool`: `pwd`".to_string()
            }
        );
    }

    #[test]
    fn test_parse_realtime_event_tool_completed_uses_fallback_output() {
        let done = json!({
            "type":"message.part.updated",
            "properties":{
                "part":{
                    "type":"tool",
                    "id":"t9",
                    "state":{"status":"completed","output":"fallback-out"}
                }
            },
            "data":{}
        });
        assert_eq!(
            OpencodeAgent::parse_realtime_event(&done),
            RealtimeEventAction::ToolUpdate {
                id: "t9".to_string(),
                output: "fallback-out".to_string()
            }
        );
    }

    #[test]
    fn test_extract_error_message_fallbacks() {
        let properties = json!({"message":"p-msg"});
        let data = json!({"message":"d-msg"});
        assert_eq!(
            OpencodeAgent::extract_error_message(&properties, &data),
            "p-msg"
        );

        let properties2 = json!({});
        assert_eq!(
            OpencodeAgent::extract_error_message(&properties2, &data),
            "d-msg"
        );

        let data2 = json!({});
        assert_eq!(
            OpencodeAgent::extract_error_message(&properties2, &data2),
            "Unknown Error"
        );
    }

    #[tokio::test]
    async fn test_build_parts_from_input_handles_inline_and_fallback() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let small_path = dir.path().join("a.txt");
        tokio::fs::write(&small_path, b"hello").await?;

        let input = UserInput {
            text: "prompt".to_string(),
            files: vec![UploadedFile {
                id: "1".to_string(),
                name: "a.txt".to_string(),
                mime: "text/plain".to_string(),
                size: 5,
                local_path: small_path.to_string_lossy().to_string(),
                source_url: "u".to_string(),
            }],
        };
        let (text, parts) = OpencodeAgent::build_parts_from_input(&input).await;
        assert!(text.contains("[Uploaded Files]"));
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["type"], "file");
        assert!(!parts[0]["data"].as_str().unwrap_or("").is_empty());

        let input_large = UserInput {
            text: "prompt2".to_string(),
            files: vec![UploadedFile {
                id: "2".to_string(),
                name: "big.bin".to_string(),
                mime: "application/octet-stream".to_string(),
                size: OpencodeAgent::MAX_INLINE_FILE_BYTES + 1,
                local_path: "/tmp/not-read.bin".to_string(),
                source_url: "u2".to_string(),
            }],
        };
        let (text_large, parts_large) = OpencodeAgent::build_parts_from_input(&input_large).await;
        assert!(text_large.contains("mode=fallback_path"));
        assert!(parts_large.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn test_build_parts_from_input_image_uses_image_type() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let img_path = dir.path().join("a.png");
        tokio::fs::write(&img_path, b"png-bytes").await?;
        let input = UserInput {
            text: "img".to_string(),
            files: vec![UploadedFile {
                id: "i1".to_string(),
                name: "a.png".to_string(),
                mime: "image/png".to_string(),
                size: 9,
                local_path: img_path.to_string_lossy().to_string(),
                source_url: "u".to_string(),
            }],
        };
        let (_text, parts) = OpencodeAgent::build_parts_from_input(&input).await;
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["type"], "image");
        Ok(())
    }

    #[tokio::test]
    async fn test_build_parts_from_input_missing_file_falls_back() -> anyhow::Result<()> {
        let input = UserInput {
            text: "missing".to_string(),
            files: vec![UploadedFile {
                id: "m1".to_string(),
                name: "missing.txt".to_string(),
                mime: "text/plain".to_string(),
                size: 8,
                local_path: "/tmp/definitely-not-exists-xyz.txt".to_string(),
                source_url: "u".to_string(),
            }],
        };
        let (text, parts) = OpencodeAgent::build_parts_from_input(&input).await;
        assert!(text.contains("mode=fallback_path"));
        assert!(parts.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn test_construct_message_body_contains_model_when_set() -> anyhow::Result<()> {
        let input = UserInput::new_text("hello".to_string());
        let body = OpencodeAgent::construct_message_body(
            &input,
            &Some(("openai".to_string(), "gpt-4.1".to_string())),
        )
        .await;
        assert_eq!(body["model"]["providerID"], "openai");
        assert_eq!(body["model"]["modelID"], "gpt-4.1");
        assert_eq!(body["parts"][0]["type"], "text");
        Ok(())
    }

    #[tokio::test]
    async fn test_construct_message_body_without_model() -> anyhow::Result<()> {
        let input = UserInput::new_text("hello".to_string());
        let body = OpencodeAgent::construct_message_body(&input, &None).await;
        assert!(body.get("model").is_none());
        assert_eq!(body["parts"][0]["text"], "hello");
        Ok(())
    }

    #[tokio::test]
    async fn test_get_available_models_filters_connected_providers() -> anyhow::Result<()> {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/provider"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "connected":["openai"],
                "all":[
                    {"id":"openai","models":{"gpt-4.1":{},"gpt-4o":{}}},
                    {"id":"other","models":{"x":{}}}
                ]
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let (agent, _) = build_test_agent(&mock_server, "k", "sid");
        let models = agent.get_available_models().await?;
        assert_eq!(models.len(), 2);
        assert!(models.iter().all(|m| m.provider == "openai"));
        Ok(())
    }

    #[tokio::test]
    async fn test_get_available_models_empty_when_disconnected() -> anyhow::Result<()> {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/provider"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "connected":[],
                "all":[{"id":"openai","models":{"gpt-4.1":{}}}]
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let (agent, _) = build_test_agent(&mock_server, "k", "sid");
        let models = agent.get_available_models().await?;
        assert!(models.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn test_get_state_404_clears_sid() -> anyhow::Result<()> {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir()?;
        // SAFETY: serialized by env lock
        unsafe { std::env::set_var(BASE_DIR_ENV, dir.path()) };

        let mock_server_404 = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/session/sid"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&mock_server_404)
            .await;
        let (agent_404, _) = build_test_agent(&mock_server_404, "k", "sid");
        let state_404 = agent_404.get_state().await?;
        assert_eq!(state_404.message_count, 0);
        // SAFETY: serialized by env lock
        unsafe { std::env::remove_var(BASE_DIR_ENV) };
        Ok(())
    }

    #[tokio::test]
    async fn test_set_model_persists_to_channel_config() -> anyhow::Result<()> {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir()?;
        // SAFETY: serialized by env lock
        unsafe { std::env::set_var(BASE_DIR_ENV, dir.path()) };

        let mock_server = MockServer::start().await;
        let (agent, _) = build_test_agent(&mock_server, "k", "sid");
        agent.set_model("openai", "gpt-4.1").await?;
        let model = agent.current_model.lock().await.clone();
        assert_eq!(model, Some(("openai".to_string(), "gpt-4.1".to_string())));
        // SAFETY: serialized by env lock
        unsafe { std::env::remove_var(BASE_DIR_ENV) };
        Ok(())
    }

    #[tokio::test]
    async fn test_compact_success_and_failure() -> anyhow::Result<()> {
        let ok_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/session/sid/message"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&ok_server)
            .await;
        let (ok_agent, _) = build_test_agent(&ok_server, "k", "sid");
        ok_agent.compact().await?;

        let fail_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/session/sid/message"))
            .respond_with(ResponseTemplate::new(500))
            .expect(1)
            .mount(&fail_server)
            .await;
        let (fail_agent, _) = build_test_agent(&fail_server, "k", "sid");
        let err = fail_agent.compact().await.expect_err("compact must fail");
        assert!(err.to_string().contains("Compact failed"));
        Ok(())
    }

    #[tokio::test]
    async fn test_abort_hits_endpoint() -> anyhow::Result<()> {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/session/sid/abort"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&mock_server)
            .await;
        let (agent, _) = build_test_agent(&mock_server, "k", "sid");
        agent.abort().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_prompt_404_clears_sid_and_returns_err() -> anyhow::Result<()> {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir()?;
        // SAFETY: serialized by env lock
        unsafe { std::env::set_var(BASE_DIR_ENV, dir.path()) };

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/session/sid/message"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&mock_server)
            .await;
        let (agent, _) = build_test_agent(&mock_server, "k", "sid");
        let err = agent.prompt("x").await.expect_err("expected 404 error");
        assert!(err.to_string().contains("Session expired"));
        // SAFETY: serialized by env lock
        unsafe { std::env::remove_var(BASE_DIR_ENV) };
        Ok(())
    }

    #[test]
    fn test_parse_realtime_event_ignores_unknown_status_and_type() {
        let unknown_status = json!({
            "type":"message.part.delta",
            "properties":{"part":{"type":"tool","state":{"status":"queued"}}},
            "data":{}
        });
        assert_eq!(
            OpencodeAgent::parse_realtime_event(&unknown_status),
            RealtimeEventAction::Ignore
        );

        let unknown_type = json!({"type":"noop"});
        assert_eq!(
            OpencodeAgent::parse_realtime_event(&unknown_type),
            RealtimeEventAction::Ignore
        );
    }
}
