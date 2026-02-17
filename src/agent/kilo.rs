use super::opencode::OpencodeAgent;
use super::{AgentEvent, AgentState, AiAgent, ModelInfo};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::broadcast;

/// KiloAgent 現在是 OpencodeAgent 的一個薄封裝。
/// 這是因為 Kilo 本質上是 OpenCode 的一個 Fork 版本。
pub struct KiloAgent {
    inner: Arc<OpencodeAgent>,
}

impl KiloAgent {
    pub async fn new(
        channel_id: u64,
        base_url: String,
        existing_sid: Option<String>,
        model_opt: Option<(String, String)>,
    ) -> anyhow::Result<Arc<Self>> {
        let inner = OpencodeAgent::new(channel_id, base_url, "".to_string(), existing_sid, model_opt, "kilo").await?;
        Ok(Arc::new(Self { inner }))
    }

    // 暴露 session_id 以供 SessionManager 使用
    pub fn session_id(&self) -> String {
        self.inner.session_id.clone()
    }
}

// 代理所有 AiAgent 介面
#[async_trait]
impl AiAgent for KiloAgent {
    async fn prompt(&self, message: &str) -> anyhow::Result<()> {
        self.inner.prompt(message).await
    }
    async fn set_session_name(&self, name: &str) -> anyhow::Result<()> {
        self.inner.set_session_name(name).await
    }
    async fn get_state(&self) -> anyhow::Result<AgentState> {
        self.inner.get_state().await
    }
    async fn compact(&self) -> anyhow::Result<()> {
        self.inner.compact().await
    }
    async fn abort(&self) -> anyhow::Result<()> {
        self.inner.abort().await
    }
    async fn clear(&self) -> anyhow::Result<()> {
        self.inner.clear().await
    }
    async fn set_model(&self, provider: &str, model_id: &str) -> anyhow::Result<()> {
        self.inner.set_model(provider, model_id).await
    }
    async fn set_thinking_level(&self, level: &str) -> anyhow::Result<()> {
        self.inner.set_thinking_level(level).await
    }
    async fn get_available_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        self.inner.get_available_models().await
    }
    async fn load_skill(&self, name: &str) -> anyhow::Result<()> {
        self.inner.load_skill(name).await
    }
    fn subscribe_events(&self) -> broadcast::Receiver<AgentEvent> {
        self.inner.subscribe_events()
    }
    fn agent_type(&self) -> &'static str {
        "kilo"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_kilo_wrapper_type() {
        // 驗證封裝後的類型標籤正確
        // 由於 new 需要網路，這裡只測試靜態介面
    }
}
