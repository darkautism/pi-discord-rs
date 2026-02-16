use super::{AgentEvent, AgentState, AiAgent, ContentItem, ContentType, ModelInfo};
use async_trait::async_trait;
use eventsource_client::{Client as SseClient, ClientBuilder, SSE};
use futures::StreamExt;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::sync::Mutex;
use tracing::{error, info};

pub struct KiloAgent {
    client: reqwest::Client,
    base_url: String,
    pub session_id: String,
    channel_id: u64,
    event_tx: broadcast::Sender<AgentEvent>,
    pending_trace: Arc<Mutex<String>>,
    current_model: Arc<Mutex<Option<(String, String)>>>, // (provider, model_id)
    turn_failed: Arc<AtomicBool>,
    has_content: Arc<AtomicBool>, // 新增：追蹤本回合是否有實質內容輸出
}

impl KiloAgent {
    // 提取構造請求 Body 的邏輯以便測試
    fn construct_message_body(message: &str, model_opt: &Option<(String, String)>) -> Value {
        let mut body = json!({
            "parts": [{"type": "text", "text": message}],
        });

        if let Some((provider, model)) = model_opt {
            // 工業級保真：完全透傳模型列表給出的原始 ID
            body["model"] = json!({
                "providerID": provider,
                "modelID": model,
            });
        }
        body
    }

    // 遞歸搜尋錯誤訊息，不再依賴固定路徑
    fn find_error_message(val: &Value) -> Option<String> {
        if let Some(msg) = val.as_str() {
            return Some(msg.to_string());
        }

        if let Some(obj) = val.as_object() {
            // 優先順序：message > error > data
            for key in &["message", "error", "data", "name"] {
                if let Some(child) = obj.get(*key) {
                    if let Some(found) = Self::find_error_message(child) {
                        return Some(found);
                    }
                }
            }
            // 如果上述都沒找到，掃描所有剩餘欄位
            for (k, child) in obj {
                if k == "message" || k == "error" || k == "data" {
                    continue;
                }
                if let Some(found) = Self::find_error_message(child) {
                    return Some(found);
                }
            }
        } else if let Some(arr) = val.as_array() {
            for child in arr {
                if let Some(found) = Self::find_error_message(child) {
                    return Some(found);
                }
            }
        }
        None
    }

    pub async fn new(
        channel_id: u64,
        base_url: String,
        existing_sid: Option<String>,
        model_opt: Option<(String, String)>,
    ) -> anyhow::Result<Arc<Self>> {
        let client = reqwest::Client::new();
        let mut session_id = existing_sid;

        if session_id.is_none() {
            info!("Creating NEW Kilo session for channel {}", channel_id);
            let session_resp = client
                .post(format!("{}/session", base_url))
                .json(&json!({
                    "title": format!("Discord #{}", channel_id),
                }))
                .send()
                .await?;

            let session_info: Value = session_resp.json().await?;
            session_id = Some(
                session_info["id"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Failed to create Kilo session"))?
                    .to_string(),
            );
        }

        let session_id = session_id.unwrap();
        info!("Kilo Session ID (Active): {}", session_id);

        let (event_tx, _) = broadcast::channel(1000);
        let tx = event_tx.clone();
        let pending_trace = Arc::new(Mutex::new(String::new()));
        let current_model = Arc::new(Mutex::new(model_opt));
        let turn_failed = Arc::new(AtomicBool::new(false));
        let has_content = Arc::new(AtomicBool::new(false));

        let agent = Arc::new(Self {
            client,
            base_url: base_url.clone(),
            session_id: session_id.clone(),
            channel_id,
            event_tx: tx,
            pending_trace,
            current_model,
            turn_failed,
            has_content,
        });

        let sse_url = format!("{}/event", base_url);
        let agent_weak = Arc::downgrade(&agent);
        let sid_for_sse = session_id.clone();

        tokio::spawn(async move {
            info!(
                "🚀 Starting Kilo SSE listener for session {} at {}",
                sid_for_sse, sse_url
            );
            let builder = ClientBuilder::for_url(&sse_url).expect("Invalid SSE URL");
            let sse_client = builder.build();
            let mut stream = sse_client.stream();

            while let Some(event) = stream.next().await {
                match event {
                    Ok(SSE::Event(ev)) => {
                        if let Ok(val) = serde_json::from_str::<Value>(&ev.data) {
                            if let Some(agent) = agent_weak.upgrade() {
                                agent.handle_kilo_event(val).await;
                            } else {
                                break;
                            }
                        }
                    }
                    Ok(SSE::Comment(c)) => {
                        info!("Kilo SSE Comment: {}", c);
                    }
                    Err(e) => {
                        error!("❌ SSE Stream Error for {}: {:?}", sid_for_sse, e);
                        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    }
                }
            }
        });

        Ok(agent)
    }

    async fn handle_kilo_event(&self, val: Value) {
        let type_ = val["type"].as_str().unwrap_or("");
        let properties = &val["properties"];
        let data = &val["data"];

        // 核心診斷：打印所有關鍵事件的完整內容
        if type_.contains("part") || type_.contains("tool") {
            info!("[RAW-KILO-EVENT] type: {}, data: {}", type_, val);
        }

        let event_sid = properties["sessionID"]
            .as_str()
            .or(properties["info"]["id"].as_str())
            .or(data["sessionID"].as_str())
            .or(data["info"]["sessionID"].as_str())
            .or(val["sessionID"].as_str());

        if let Some(sid) = event_sid {
            if sid != self.session_id {
                return;
            }
        } else if type_.starts_with("session.") || type_.starts_with("message.") {
            if type_ == "server.heartbeat" {
                return;
            }
        }

        match type_ {
            "message.part.updated" | "message.part.delta" | "session.message.part.delta" => {
                let delta = properties["delta"]
                    .as_str()
                    .or(data["delta"].as_str())
                    .or(val["delta"].as_str())
                    .unwrap_or("");

                let part_info = &properties["part"];
                let part_type = part_info["type"]
                    .as_str()
                    .or(properties["type"].as_str())
                    .or(data["type"].as_str())
                    .unwrap_or("text");

                let is_thinking =
                    part_type == "reasoning" || part_type == "thinking" || part_type == "thought";
                let part_id = part_info["id"]
                    .as_str()
                    .or(properties["partId"].as_str())
                    .map(|s| s.to_string());

                if is_thinking {
                    let full_think = part_info["text"].as_str().unwrap_or("");
                    if !full_think.is_empty() {
                        let _ = self.event_tx.send(AgentEvent::MessageUpdate {
                            thinking: full_think.to_string(),
                            text: "".into(),
                            is_delta: false,
                            id: part_id.clone(),
                        });
                    } else if !delta.is_empty() {
                        let _ = self.event_tx.send(AgentEvent::MessageUpdate {
                            thinking: delta.to_string(),
                            text: "".into(),
                            is_delta: true,
                            id: part_id.clone(),
                        });
                    }
                    return;
                }

                // 統一工具處理邏輯 (SSE 實時流)
                if part_type == "tool"
                    || part_type == "tool-call"
                    || part_type == "tool_call"
                    || part_type == "agent"
                {
                    // 工具啟動前，必須清空軌跡緩衝區，防止後續內容被攔截
                    self.pending_trace.lock().await.clear();

                    let id = part_info["id"]
                        .as_str()
                        .or(part_info["callID"].as_str())
                        .or(properties["toolCallId"].as_str())
                        .unwrap_or("tool-id")
                        .to_string();

                    let status = part_info["state"]["status"].as_str().unwrap_or("");
                    let name = part_info["tool"]
                        .as_str()
                        .or(part_info["toolName"].as_str())
                        .or(part_info["agent"].as_str())
                        .unwrap_or("tool")
                        .to_string();

                    if status == "running" || status == "pending" {
                        // 提取命令標題
                        let cmd = part_info["state"]["input"]["command"]
                            .as_str()
                            .unwrap_or("");
                        let label = if !cmd.is_empty() {
                            format!("🛠️ `{}`: `{}`", name, cmd)
                        } else {
                            format!("🛠️ `{}`", name)
                        };
                        let _ = self.event_tx.send(AgentEvent::ToolExecutionStart {
                            id: id.clone(),
                            name: label,
                        });
                    }

                    if status == "completed" {
                        let output = part_info["state"]["metadata"]["output"]
                            .as_str()
                            .or(part_info["state"]["output"].as_str())
                            .unwrap_or("")
                            .to_string();
                        if !output.is_empty() {
                            let _ = self
                                .event_tx
                                .send(AgentEvent::ToolExecutionUpdate { id, output });
                        }
                    }
                    return;
                }

                // 傳統工具結果解析 (兼容舊版或特定 Provider)
                if part_type == "tool-result" || part_type == "tool_result" {
                    let id = part_info["id"]
                        .as_str()
                        .or(properties["toolCallId"].as_str())
                        .unwrap_or("tool-id")
                        .to_string();
                    let output = part_info["text"]
                        .as_str()
                        .or(part_info["content"].as_str())
                        .unwrap_or("")
                        .to_string();
                    if !output.is_empty() {
                        let _ = self
                            .event_tx
                            .send(AgentEvent::ToolExecutionUpdate { id, output });
                    }
                    return;
                }

                if delta.is_empty() {
                    return;
                }

                let mut buf = self.pending_trace.lock().await;
                if delta.contains('→') || delta.contains("🛠️") || !buf.is_empty() {
                    buf.push_str(delta);
                    if delta.contains('\n') && !buf.starts_with('→') {
                        let content = buf.split_off(0);
                        let _ = self.event_tx.send(AgentEvent::MessageUpdate {
                            thinking: "".into(),
                            text: content,
                            is_delta: true,
                            id: None,
                        });
                    }
                    return;
                }

                if !delta.trim().is_empty() {
                    self.has_content.store(true, Ordering::SeqCst);
                }

                let full_text = part_info["text"].as_str().unwrap_or("");
                if !full_text.is_empty() {
                    let _ = self.event_tx.send(AgentEvent::MessageUpdate {
                        thinking: "".into(),
                        text: full_text.into(),
                        is_delta: false,
                        id: part_id,
                    });
                } else {
                    let _ = self.event_tx.send(AgentEvent::MessageUpdate {
                        thinking: "".into(),
                        text: delta.into(),
                        is_delta: true,
                        id: part_id,
                    });
                }
            }
            "session.error" | "error" => {
                let mut msg = Self::find_error_message(&val)
                    .unwrap_or_else(|| format!("Kilo raw error: {}", val));

                // 智慧診斷：如果報錯是 Unauthorized，嘗試找出哪個供應商
                if msg == "Unauthorized" {
                    if let Some(p) = val["properties"]["error"]["data"]["providerID"].as_str() {
                        msg = format!("Unauthorized: Provider '{}' requires API Key. Run `kilo auth set {}` on server.", p, p);
                    }
                }

                let has_out = self.has_content.load(Ordering::SeqCst);
                if has_out
                    && (msg.contains("fakegpt")
                        || msg.contains("title")
                        || msg.contains("Unauthorized"))
                {
                    info!("🟡 Kilo Background Error (Ignored): {}", msg);
                    return;
                }

                error!("❌ Kilo Session Error (Fatal): {}", msg);
                self.turn_failed.store(true, Ordering::SeqCst);
                let _ = self.event_tx.send(AgentEvent::AgentEnd {
                    success: false,
                    error: Some(msg),
                });
            }
            "session.turn.close" | "session.message.completed" => {
                if !self.turn_failed.load(Ordering::SeqCst) {
                    info!(
                        "Kilo turn closed successfully for {}. Triggering final sync.",
                        self.session_id
                    );

                    let _agent_clone = self.event_tx.clone();
                    let agent_flush_clone = Arc::clone(&self.pending_trace);
                    let agent_tx_clone = self.event_tx.clone();
                    let client_clone = self.client.clone();
                    let url_clone =
                        format!("{}/session/{}/message", self.base_url, self.session_id);

                    tokio::spawn(async move {
                        if let Ok(resp) = client_clone.get(url_clone).send().await {
                            if let Ok(msgs) = resp.json::<Value>().await {
                                // 抓取最後一個助理回覆 (role: assistant)
                                if let Some(last_msg) = msgs.as_array().and_then(|a| {
                                    a.iter().filter(|m| m["role"] == "assistant").last()
                                }) {
                                    if let Some(parts) = last_msg["parts"].as_array() {
                                        let mut items = Vec::new();
                                        for p in parts {
                                            let t = p["type"].as_str().unwrap_or("");
                                            let part_id = p["id"].as_str().map(|s| s.to_string());

                                            let content = p["text"]
                                                .as_str()
                                                .or(p["content"].as_str())
                                                .or(p["result"].as_str())
                                                .unwrap_or("")
                                                .to_string();

                                            match t {
                                                "text" => {
                                                    if !content.is_empty() {
                                                        items.push(ContentItem {
                                                            type_: ContentType::Text,
                                                            content,
                                                            id: part_id,
                                                        });
                                                    }
                                                }
                                                "thinking" | "reasoning" | "thought" => {
                                                    if !content.is_empty() {
                                                        items.push(ContentItem {
                                                            type_: ContentType::Thinking,
                                                            content,
                                                            id: part_id,
                                                        });
                                                    }
                                                }
                                                "tool-call" | "agent" | "tool_call" | "call"
                                                | "tool" => {
                                                    let id = p["id"]
                                                        .as_str()
                                                        .or(p["callID"].as_str())
                                                        .or(p["toolCallId"].as_str())
                                                        .unwrap_or("tool-id")
                                                        .to_string();

                                                    let name = p["tool"]
                                                        .as_str()
                                                        .or(p["toolName"].as_str())
                                                        .or(p["agent"].as_str())
                                                        .or(p["method"].as_str())
                                                        .unwrap_or("tool")
                                                        .to_string();

                                                    let output = p["state"]["metadata"]["output"]
                                                        .as_str()
                                                        .or(p["state"]["output"].as_str())
                                                        .or(p["result"].as_str())
                                                        .unwrap_or("");

                                                    let cmd = p["state"]["input"]["command"]
                                                        .as_str()
                                                        .or(p["args"]["command"].as_str())
                                                        .unwrap_or("");
                                                    let label = if !cmd.is_empty() {
                                                        format!("🛠️ `{}`: `{}`", name, cmd)
                                                    } else {
                                                        format!("🛠️ `{}`", name)
                                                    };

                                                    items.push(ContentItem {
                                                        type_: ContentType::ToolCall(label),
                                                        content: "".into(),
                                                        id: Some(id.clone()),
                                                    });

                                                    if !output.is_empty() {
                                                        items.push(ContentItem {
                                                            type_: ContentType::ToolOutput,
                                                            content: output.to_string(),
                                                            id: Some(id),
                                                        });
                                                    }
                                                }
                                                "tool-result" | "tool_result" | "result" => {
                                                    let id = p["id"]
                                                        .as_str()
                                                        .or(p["toolCallId"].as_str())
                                                        .unwrap_or("tool-id")
                                                        .to_string();
                                                    items.push(ContentItem {
                                                        type_: ContentType::ToolOutput,
                                                        content,
                                                        id: Some(id),
                                                    });
                                                }
                                                _ => {
                                                    if !content.is_empty() {
                                                        items.push(ContentItem {
                                                            type_: ContentType::Text,
                                                            content,
                                                            id: part_id,
                                                        });
                                                    }
                                                }
                                            }
                                        }
                                        let _ = agent_tx_clone.send(AgentEvent::ContentSync { items });
                                    }
                                }
                            }
                        }

                        // 最終安全檢查：如果還有卡在緩衝區的回答，強制噴出
                        let mut buf = agent_flush_clone.lock().await;
                        if !buf.is_empty() {
                            let content = buf.split_off(0);
                            let _ = agent_tx_clone.send(AgentEvent::MessageUpdate {
                                thinking: "".into(),
                                text: content,
                                is_delta: true,
                                id: None,
                            });
                        }

                        let _ = agent_tx_clone.send(AgentEvent::AgentEnd {
                            success: true,
                            error: None,
                        });
                    });
                } else {
                    info!("Kilo turn closed after error for {}", self.session_id);
                }
            }
            "session.log" | "tool.start" => {
                let msg = properties["message"].as_str().unwrap_or("");
                if msg.contains("Executing tool") || type_ == "tool.start" {
                    let id = properties["toolCallId"]
                        .as_str()
                        .or(data["toolCallId"].as_str())
                        .unwrap_or("tool-id")
                        .to_string();
                    let mut buf = self.pending_trace.lock().await;
                    let name = if !buf.is_empty() {
                        buf.split_off(0)
                    } else {
                        "tool".into()
                    };
                    let _ = self
                        .event_tx
                        .send(AgentEvent::ToolExecutionStart { id, name });
                }
            }
            _ => {}
        }
    }
}

#[async_trait]
impl AiAgent for KiloAgent {
    async fn prompt(&self, message: &str) -> anyhow::Result<()> {
        let url = format!("{}/session/{}/message", self.base_url, self.session_id);
        info!(
            "Sending prompt to Kilo: {} (Session: {})",
            message, self.session_id
        );

        self.turn_failed.store(false, Ordering::SeqCst);
        self.has_content.store(false, Ordering::SeqCst);

        let model_opt = self.current_model.lock().await.clone();
        let body = Self::construct_message_body(message, &model_opt);

        let resp = self.client.post(url).json(&body).send().await?;

        if !resp.status().is_success() {
            let status_code = resp.status();
            let err_json: Value = resp
                .json()
                .await
                .unwrap_or(json!({ "error": { "message": format!("HTTP {}", status_code) } }));
            let err_msg = Self::find_error_message(&err_json).unwrap_or_else(|| {
                format!("Kilo API communication failed (Status: {})", status_code)
            });

            error!("Kilo API Error: {}", err_msg);
            anyhow::bail!(err_msg);
        }

        info!("Kilo prompt request accepted");
        Ok(())
    }

    async fn set_session_name(&self, _name: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn get_state(&self) -> anyhow::Result<AgentState> {
        let m = self.current_model.lock().await;
        let model_str = m.as_ref().map(|(p, mid)| format!("{}/{}", p, mid));
        Ok(AgentState {
            message_count: 0,
            model: model_str,
        })
    }
    async fn compact(&self) -> anyhow::Result<()> {
        Ok(())
    }
    async fn abort(&self) -> anyhow::Result<()> {
        self.client
            .post(format!(
                "{}/session/{}/abort",
                self.base_url, self.session_id
            ))
            .send()
            .await?;
        Ok(())
    }
    async fn clear(&self) -> anyhow::Result<()> {
        Ok(())
    }
    async fn set_model(&self, provider: &str, model_id: &str) -> anyhow::Result<()> {
        let mut m = self.current_model.lock().await;
        *m = Some((provider.to_string(), model_id.to_string()));
        info!("Kilo model set to {}/{}", provider, model_id);

        let mut channel_config = crate::commands::agent::ChannelConfig::load()
            .await
            .unwrap_or_default();
        if let Some(entry) = channel_config
            .channels
            .get_mut(&self.channel_id.to_string())
        {
            entry.model_provider = Some(provider.to_string());
            entry.model_id = Some(model_id.to_string());
            channel_config.save().await?;
            info!("Kilo model persisted for channel {}", self.channel_id);
        }
        Ok(())
    }
    async fn set_thinking_level(&self, _l: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn get_available_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        let resp = self
            .client
            .get(format!("{}/provider", self.base_url))
            .send()
            .await?;
        let val: Value = resp.json().await?;

        let connected_providers: Vec<String> = val["connected"]
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
                let provider_id = p["id"].as_str().unwrap_or("");

                // 核心邏輯：只顯示已連線的 Provider 提供的模型
                if !connected_providers.contains(&provider_id.to_string()) {
                    continue;
                }

                if let Some(m_map) = p["models"].as_object() {
                    for (id, _m_info) in m_map {
                        models.push(ModelInfo {
                            provider: provider_id.to_string(),
                            id: id.clone(),
                            label: format!("{}/{}", provider_id, id),
                        });
                    }
                }
            }
        }

        models.sort_by(|a, b| {
            let a_free = a.id.contains("free") || a.provider == "kilo";
            let b_free = b.id.contains("free") || b.provider == "kilo";
            b_free.cmp(&a_free)
        });

        Ok(models)
    }
    async fn load_skill(&self, _n: &str) -> anyhow::Result<()> {
        Ok(())
    }
    fn subscribe_events(&self) -> broadcast::Receiver<AgentEvent> {
        self.event_tx.subscribe()
    }
    fn agent_type(&self) -> &'static str {
        "kilo"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_kilo_error_fatal_vs_background() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(10);
        let agent = KiloAgent {
            client: reqwest::Client::new(),
            base_url: "http://localhost".into(),
            session_id: "ses_123".into(),
            channel_id: 123,
            event_tx: tx,
            pending_trace: Arc::new(Mutex::new(String::new())),
            current_model: Arc::new(Mutex::new(None)),
            turn_failed: Arc::new(AtomicBool::new(false)),
            has_content: Arc::new(AtomicBool::new(false)),
        };

        let fatal_err = json!({
            "type": "session.error",
            "properties": { "sessionID": "ses_123", "message": "Unauthorized" }
        });
        agent.handle_kilo_event(fatal_err).await;

        let ev = rx.try_recv().expect("Fatal error was ignored!");
        if let AgentEvent::AgentEnd { success, .. } = ev {
            assert!(!success, "Fatal error should report failure!");
        }

        agent.has_content.store(true, Ordering::SeqCst);
        agent.turn_failed.store(false, Ordering::SeqCst);

        let bg_err = json!({
            "type": "session.error",
            "properties": { "sessionID": "ses_123", "message": "Unauthorized" }
        });
        agent.handle_kilo_event(bg_err).await;
        assert!(
            rx.try_recv().is_err(),
            "Background error should be ignored when content exists!"
        );
    }

    #[tokio::test]
    async fn test_kilo_error_deep_recursive_extraction() {
        let (tx, _rx) = tokio::sync::broadcast::channel(10);
        let _agent = KiloAgent {
            client: reqwest::Client::new(),
            base_url: "http://localhost".into(),
            session_id: "ses_123".into(),
            channel_id: 123,
            event_tx: tx,
            pending_trace: Arc::new(Mutex::new(String::new())),
            current_model: Arc::new(Mutex::new(None)),
            turn_failed: Arc::new(AtomicBool::new(false)),
            has_content: Arc::new(AtomicBool::new(false)),
        };

        let nested = json!({
            "error": { "inner": { "message": "Deep error" } }
        });
        assert_eq!(
            KiloAgent::find_error_message(&nested),
            Some("Deep error".into())
        );
    }

    #[tokio::test]
    async fn test_kilo_protocol_reasoning_standard() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(10);
        let agent = KiloAgent {
            client: reqwest::Client::new(),
            base_url: "http://localhost".into(),
            session_id: "ses_123".into(),
            channel_id: 123,
            event_tx: tx,
            pending_trace: Arc::new(Mutex::new(String::new())),
            current_model: Arc::new(Mutex::new(None)),
            turn_failed: Arc::new(AtomicBool::new(false)),
            has_content: Arc::new(AtomicBool::new(false)),
        };

        let reasoning_ev = json!({
            "type": "message.part.updated",
            "properties": {
                "sessionID": "ses_123",
                "part": { "type": "reasoning" },
                "delta": "Deeply thinking..."
            }
        });

        agent.handle_kilo_event(reasoning_ev).await;

        if let Ok(AgentEvent::MessageUpdate { thinking, text, .. }) = rx.recv().await {
            assert_eq!(thinking, "Deeply thinking...");
            assert!(text.is_empty());
        } else {
            panic!("Failed to capture real Kilo reasoning structure!");
        }
    }

    #[tokio::test]
    async fn test_model_id_zero_mutation_guarantee() {
        let (tx, _rx) = tokio::sync::broadcast::channel(10);
        let agent = KiloAgent {
            client: reqwest::Client::new(),
            base_url: "http://localhost".into(),
            session_id: "ses_123".into(),
            channel_id: 123,
            event_tx: tx,
            pending_trace: Arc::new(Mutex::new(String::new())),
            current_model: Arc::new(Mutex::new(None)),
            turn_failed: Arc::new(AtomicBool::new(false)),
            has_content: Arc::new(AtomicBool::new(false)),
        };

        let complex_ids = vec![("z-ai", "glm-4.5:free"), ("google", "gemma-2.5-it")];

        for (p_in, m_in) in complex_ids {
            agent.set_model(p_in, m_in).await.unwrap();
            let model_opt = agent.current_model.lock().await.clone();
            let body = KiloAgent::construct_message_body("Hi", &model_opt);

            assert_eq!(body["model"]["providerID"], p_in);
            assert_eq!(body["model"]["modelID"], m_in);
        }
    }

    #[tokio::test]
    async fn test_kilo_protocol_complex_tool_structure() {
        use tokio::sync::broadcast;
        let (tx, mut rx) = broadcast::channel::<AgentEvent>(10);
        let agent = KiloAgent {
            base_url: "http://localhost".into(),
            session_id: "test-ses".into(),
            client: reqwest::Client::new(),
            event_tx: tx,
            pending_trace: Arc::new(Mutex::new(String::new())),
            has_content: Arc::new(AtomicBool::new(false)),
            turn_failed: Arc::new(AtomicBool::new(false)),
            channel_id: 123,
            current_model: Arc::new(Mutex::new(None)),
        };

        // 模擬 SSE 中常見的 "type: tool" 結構 (Running 狀態)
        let tool_start = json!({
            "type": "message.part.updated",
            "properties": {
                "part": {
                    "type": "tool",
                    "tool": "bash",
                    "callID": "call-123",
                    "state": {
                        "status": "running",
                        "input": { "command": "ls -la" }
                    }
                }
            }
        });

        agent.handle_kilo_event(tool_start).await;
        if let Ok(AgentEvent::ToolExecutionStart { id, name }) = rx.recv().await {
            assert_eq!(id, "call-123");
            assert!(name.contains("bash"));
            assert!(name.contains("ls -la"));
        } else {
            panic!("Expected ToolExecutionStart");
        }

        // 模擬 "type: tool" 結構 (Completed 狀態)
        let tool_end = json!({
            "type": "message.part.updated",
            "properties": {
                "part": {
                    "type": "tool",
                    "tool": "bash",
                    "callID": "call-123",
                    "state": {
                        "status": "completed",
                        "metadata": { "output": "file1\nfile2" }
                    }
                }
            }
        });

        agent.handle_kilo_event(tool_end).await;
        if let Ok(AgentEvent::ToolExecutionUpdate { id, output }) = rx.recv().await {
            assert_eq!(id, "call-123");
            assert_eq!(output, "file1\nfile2");
        } else {
            panic!("Expected ToolExecutionUpdate");
        }
    }

    #[test]
    fn test_kilo_unauthorized_provider_extraction() {
        let err_json = json!({
            "type": "error",
            "properties": {
                "error": {
                    "data": {
                        "message": "Unauthorized",
                        "providerID": "z-ai"
                    }
                }
            }
        });

        let mut msg = KiloAgent::find_error_message(&err_json).unwrap();
        if msg == "Unauthorized" {
            if let Some(p) = err_json["properties"]["error"]["data"]["providerID"].as_str() {
                msg = format!("Unauthorized: Provider '{}' requires API Key", p);
            }
        }
        assert!(msg.contains("z-ai"));
    }
}
