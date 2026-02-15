use super::{AgentEvent, AgentState, AiAgent, ModelInfo};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tracing::{error, info};

#[derive(Clone)]
pub struct OpencodeConfig {
    pub host: String,
    pub port: u16,
    pub password: Option<String>,
}

pub struct OpencodeAgent {
    client: reqwest::Client,
    base_url: String,
    session_id: Arc<RwLock<String>>,
    auth: Option<(String, String)>,
    event_tx: broadcast::Sender<AgentEvent>,
    channel_id: u64,
}

#[derive(Deserialize)]
struct Session {
    id: String,
}

#[derive(Serialize)]
struct CreateSessionRequest {
    title: String,
}

impl OpencodeAgent {
    pub async fn new(channel_id: u64, config: &OpencodeConfig) -> anyhow::Result<Arc<Self>> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .connect_timeout(std::time::Duration::from_secs(5))
            .build()
            .map_err(|e| anyhow::anyhow!("無法建立 HTTP client: {}", e))?;
        
        let base_url = format!("http://{}:{}", config.host, config.port);
        info!("🔄 正在連線至 OpenCode server: {}", base_url);

        let auth = config.password.as_ref().map(|p| ("opencode".to_string(), p.clone()));

        // 創建 session
        let session = Self::create_session(&client, &base_url, &auth, channel_id).await
            .map_err(|e| {
                error!("❌ OpenCode 連線失敗: {}", e);
                anyhow::anyhow!("無法連線至 OpenCode server ({}): {}。請確認 opencode serve 是否已啟動", base_url, e)
            })?;

        let event_tx = broadcast::channel(100).0;

        let agent = Arc::new(OpencodeAgent {
            client,
            base_url: base_url.clone(),
            session_id: Arc::new(RwLock::new(session.id.clone())),
            auth,
            event_tx: event_tx.clone(),
            channel_id,
        });

        // 啟動事件監聽
        let agent_clone = agent.clone();
        tokio::spawn(async move {
            agent_clone.event_listener().await;
        });

        info!("🚀 Connected to OpenCode server for channel {}: session {}", channel_id, session.id);

        Ok(agent)
    }

    async fn create_session(
        client: &reqwest::Client,
        base_url: &str,
        auth: &Option<(String, String)>,
        channel_id: u64,
    ) -> anyhow::Result<Session> {
        let url = format!("{}/session", base_url);
        let body = json!({
            "title": format!("discord-rs-{}", channel_id)
        });

        let mut req = client.post(&url).json(&body);
        if let Some((user, pass)) = auth {
            req = req.basic_auth(user, Some(pass));
        }

        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("HTTP {}: {}", status, text);
        }

        let session: Session = resp.json().await?;
        Ok(session)
    }

    async fn event_listener(&self) {
        // 簡化版本：使用輪詢方式獲取訊息
        // 實際生產環境應使用 SSE stream
        info!("OpenCode event listener started for channel {}", self.channel_id);
        
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            
            // 這裡可以實作輪詢邏輯
            // 目前簡化為空迴圈，實際訊息在 prompt 後通過 HTTP response 返回
        }
    }

    fn parse_event(&self, val: Value) {
        // OpenCode 的事件格式與 Pi 不同，需要轉換
        // 這裡是簡化版本，需要根據實際 OpenCode 事件格式調整
        match val["type"].as_str() {
            Some("message") => {
                if let Some(content) = val["content"].as_str() {
                    let _ = self.event_tx.send(AgentEvent::MessageUpdate {
                        thinking: String::new(),
                        text: content.to_string(),
                        is_delta: false,
                    });
                }
            }
            Some("tool_start") => {
                if let Some(name) = val["tool"].as_str() {
                    let _ = self.event_tx.send(AgentEvent::ToolExecutionStart {
                        name: name.to_string(),
                    });
                }
            }
            Some("tool_end") => {
                if let Some(name) = val["tool"].as_str() {
                    let _ = self.event_tx.send(AgentEvent::ToolExecutionEnd {
                        name: name.to_string(),
                    });
                }
            }
            Some("end") => {
                let success = val["error"].is_null();
                let error = val["error"].as_str().map(|s| s.to_string());
                let _ = self.event_tx.send(AgentEvent::AgentEnd { success, error });
            }
            Some("error") => {
                if let Some(msg) = val["message"].as_str() {
                    let _ = self.event_tx.send(AgentEvent::Error {
                        message: msg.to_string(),
                    });
                }
            }
            _ => {}
        }
    }

    async fn api_call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> anyhow::Result<Value> {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self.client.request(method, &url);

        if let Some((user, pass)) = &self.auth {
            req = req.basic_auth(user, Some(pass));
        }

        if let Some(b) = body {
            req = req.json(&b);
        }

        let resp = req.send().await?;
        let status = resp.status();

        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("HTTP {}: {}", status, text);
        }

        let val: Value = resp.json().await?;
        Ok(val)
    }

    pub async fn recreate_session(&self) -> anyhow::Result<()> {
        let session = Self::create_session(
            &self.client,
            &self.base_url,
            &self.auth,
            self.channel_id,
        ).await?;

        let new_id = session.id.clone();
        let mut session_id = self.session_id.write().await;
        *session_id = session.id;
        drop(session_id);

        info!("🔄 Recreated OpenCode session for channel {}: {}", self.channel_id, new_id);
        Ok(())
    }
}

#[async_trait]
impl AiAgent for OpencodeAgent {
    async fn prompt(&self, message: &str) -> anyhow::Result<()> {
        let session_id = self.session_id.read().await.clone();
        let path = format!("/session/{}/prompt", session_id);

        let resp = self.api_call(
            reqwest::Method::POST,
            &path,
            Some(json!({
                "parts": [{"type": "text", "text": message}]
            })),
        ).await?;

        // OpenCode 返回的格式通常在 content 或 message["content"]
        let content = resp["content"].as_str()
            .or_else(|| resp["message"]["content"].as_str());

        if let Some(text) = content {
            let _ = self.event_tx.send(AgentEvent::MessageUpdate {
                thinking: String::new(),
                text: text.to_string(),
                is_delta: false,
            });
        }
        
        let _ = self.event_tx.send(AgentEvent::AgentEnd {
            success: true,
            error: None,
        });

        Ok(())
    }

    async fn set_session_name(&self, name: &str) -> anyhow::Result<()> {
        let session_id = self.session_id.read().await.clone();
        let path = format!("/session/{}", session_id);

        self.api_call(
            reqwest::Method::PATCH,
            &path,
            Some(json!({ "title": name })),
        ).await?;

        Ok(())
    }

    async fn get_state(&self) -> anyhow::Result<AgentState> {
        let session_id = self.session_id.read().await.clone();
        let path = format!("/session/{}", session_id);

        let resp = self.api_call(reqwest::Method::GET, &path, None).await?;

        Ok(AgentState {
            message_count: resp["messageCount"].as_u64().unwrap_or(0),
            model: resp["model"].as_str().map(|s| s.to_string()),
        })
    }

    async fn compact(&self) -> anyhow::Result<()> {
        let session_id = self.session_id.read().await.clone();
        let path = format!("/session/{}/summarize", session_id);

        self.api_call(reqwest::Method::POST, &path, None).await?;
        Ok(())
    }

    async fn abort(&self) -> anyhow::Result<()> {
        let session_id = self.session_id.read().await.clone();
        let path = format!("/session/{}/abort", session_id);

        self.api_call(reqwest::Method::POST, &path, None).await?;
        Ok(())
    }

    async fn clear(&self) -> anyhow::Result<()> {
        // 刪除舊 session 並創建新 session
        let old_session_id = self.session_id.read().await.clone();
        let path = format!("/session/{}", old_session_id);

        let _ = self.api_call(reqwest::Method::DELETE, &path, None).await;

        self.recreate_session().await?;
        Ok(())
    }

    async fn set_model(&self, provider: &str, model_id: &str) -> anyhow::Result<()> {
        let session_id = self.session_id.read().await.clone();
        let path = format!("/session/{}/message", session_id);

        // OpenCode 使用 command 來切換模型
        self.api_call(
            reqwest::Method::POST,
            &path,
            Some(json!({
                "command": "model",
                "arguments": [format!("{}/{}", provider, model_id)]
            })),
        ).await?;

        Ok(())
    }

    async fn set_thinking_level(&self, level: &str) -> anyhow::Result<()> {
        let session_id = self.session_id.read().await.clone();
        let path = format!("/session/{}/message", session_id);

        self.api_call(
            reqwest::Method::POST,
            &path,
            Some(json!({
                "command": "thinking",
                "arguments": [level]
            })),
        ).await?;

        Ok(())
    }

    async fn get_available_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        let resp = self.api_call(
            reqwest::Method::GET,
            "/config/providers",
            None,
        ).await?;

        let mut models = Vec::new();

        if let Some(providers) = resp["providers"].as_array() {
            for provider in providers {
                let provider_name = provider["id"].as_str().unwrap_or("unknown");
                if let Some(provider_models) = provider["models"].as_array() {
                    for model in provider_models {
                        let model_id = model["id"].as_str().unwrap_or("unknown");
                        models.push(ModelInfo {
                            provider: provider_name.to_string(),
                            id: model_id.to_string(),
                            label: format!("{}/{}", provider_name, model_id),
                        });
                    }
                }
            }
        }

        Ok(models)
    }

    async fn load_skill(&self, name: &str) -> anyhow::Result<()> {
        let session_id = self.session_id.read().await.clone();
        let path = format!("/session/{}/message", session_id);

        self.api_call(
            reqwest::Method::POST,
            &path,
            Some(json!({
                "command": "skill",
                "arguments": [name]
            })),
        ).await?;

        Ok(())
    }

    fn subscribe_events(&self) -> broadcast::Receiver<AgentEvent> {
        self.event_tx.subscribe()
    }

    fn agent_type(&self) -> &'static str {
        "opencode"
    }
}

// Base64 encode helper - simple implementation
fn base64_encode(input: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(input.as_bytes())
}
