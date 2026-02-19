use super::{AgentEvent, AgentState, AiAgent, ContentItem, ContentType, ModelInfo};
use async_trait::async_trait;
use eventsource_client::{Client, ClientBuilder, SSE};
use futures::StreamExt;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::sync::Mutex;
use tracing::{error, info};

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

    fn construct_message_body(message: &str, model_opt: &Option<(String, String)>) -> Value {
        let mut body = json!({ "parts": [{"type": "text", "text": message}] });
        if let Some((provider, model)) = model_opt {
            body["model"] = json!({ "providerID": provider, "modelID": model });
        }
        body
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

        let properties = &val["properties"];
        let data = &val["data"];

        match type_ {
            "message.part.updated" | "message.part.delta" | "session.message.part.delta" => {
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

                // 核心過濾：只允許 assistant 角色或正在思考的內容
                let role = properties["messageRole"]
                    .as_str()
                    .or(data["messageRole"].as_str())
                    .or(properties["role"].as_str())
                    .or(data["role"].as_str())
                    .or(part_info["role"].as_str())
                    .unwrap_or("");

                // 如果明確是 system/user 角色且不是思考，就跳過
                if (role == "system" || role == "user")
                    && !part_type.contains("reason")
                    && !part_type.contains("think")
                {
                    return;
                }

                if part_type.contains("reason") || part_type.contains("think") {
                    let _ = self.event_tx.send(AgentEvent::MessageUpdate {
                        thinking: delta.into(),
                        text: "".into(),
                        is_delta: true,
                        id: part_id,
                    });
                } else if part_type.contains("tool") || part_type == "agent" {
                    let id = part_id.unwrap_or_else(|| "tool".into());
                    let status = part_info["state"]["status"].as_str().unwrap_or("");
                    if status == "running" || status == "pending" {
                        let name = part_info["tool"].as_str().unwrap_or("tool");
                        let cmd = part_info["state"]["input"]["command"]
                            .as_str()
                            .unwrap_or("");
                        let _ = self.event_tx.send(AgentEvent::ToolExecutionStart {
                            id,
                            name: format!("🛠️ `{}`: `{}`", name, cmd),
                        });
                    } else if status == "completed" {
                        let output = part_info["state"]["metadata"]["output"]
                            .as_str()
                            .or(part_info["state"]["output"].as_str())
                            .unwrap_or("");
                        let _ = self.event_tx.send(AgentEvent::ToolExecutionUpdate {
                            id,
                            output: output.into(),
                        });
                    }
                } else {
                    let _ = self.event_tx.send(AgentEvent::MessageUpdate {
                        thinking: "".into(),
                        text: delta.into(),
                        is_delta: true,
                        id: part_id,
                    });
                }
            }
            "session.turn.close"
            | "session.message.completed"
            | "turn.close"
            | "message.completed"
            | "turn.end"
            | "session.idle" => {
                info!("🏁 Turn completed signal received: {}", type_);
                if !self.turn_failed.load(Ordering::SeqCst) {
                    self.trigger_sync().await;
                }
            }
            "session.error" | "error" => {
                error!("❌ FULL ERROR JSON: {}", val);

                // 嘗試從嵌套結構中提取最有用的錯誤訊息
                let msg = properties["error"]["data"]["message"]
                    .as_str()
                    .or(properties["message"].as_str())
                    .or(data["message"].as_str())
                    .unwrap_or("Unknown Error");

                error!("❌ Backend Error Summary: {}", msg);
                self.turn_failed.store(true, Ordering::SeqCst);
                let _ = self.event_tx.send(AgentEvent::AgentEnd {
                    success: false,
                    error: Some(msg.into()),
                });
            }
            _ => {}
        }
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
        let url = format!("{}/session/{}/message", self.base_url, self.session_id);
        self.turn_failed.store(false, Ordering::SeqCst);
        let model_opt = self.current_model.lock().await.clone();
        let body = Self::construct_message_body(message, &model_opt);

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
                        let mut config = crate::commands::agent::ChannelConfig::load().await?;
                        if let Some(entry) = config.channels.get_mut(&self.channel_id.to_string()) {
                            entry.session_id = None;
                            if let Err(e) = config.save().await {
                                error!("❌ Failed to clear expired session id: {}", e);
                            }
                        }
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
            let mut config = crate::commands::agent::ChannelConfig::load().await?;
            if let Some(entry) = config.channels.get_mut(&self.channel_id.to_string()) {
                entry.session_id = None;
                if let Err(e) = config.save().await {
                    error!("❌ Failed to clear missing session id: {}", e);
                }
            }
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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
}
