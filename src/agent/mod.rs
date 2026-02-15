use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

#[derive(Clone, Debug)]
pub struct AgentState {
    pub message_count: u64,
    pub model: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ModelInfo {
    pub provider: String,
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug)]
pub enum AgentEvent {
    MessageUpdate {
        thinking: String,
        text: String,
        is_delta: bool,
    },
    ToolExecutionStart {
        name: String,
    },
    ToolExecutionEnd {
        name: String,
    },
    AgentEnd {
        success: bool,
        error: Option<String>,
    },
    ToolExecutionUpdate {
        output: String,
    },
    AutoRetry {
        attempt: u64,
        max: u64,
    },
    Error {
        message: String,
    },
    ConnectionError {
        message: String,
    },
    /// Response to a command (e.g., get_available_models)
    CommandResponse {
        id: String,
        data: serde_json::Value,
    },
}

#[async_trait]
pub trait AiAgent: Send + Sync {
    async fn prompt(&self, message: &str) -> anyhow::Result<()>;
    async fn set_session_name(&self, name: &str) -> anyhow::Result<()>;
    async fn get_state(&self) -> anyhow::Result<AgentState>;
    async fn compact(&self) -> anyhow::Result<()>;
    async fn abort(&self) -> anyhow::Result<()>;
    async fn clear(&self) -> anyhow::Result<()>;
    async fn set_model(&self, provider: &str, model_id: &str) -> anyhow::Result<()>;
    async fn set_thinking_level(&self, level: &str) -> anyhow::Result<()>;
    async fn get_available_models(&self) -> anyhow::Result<Vec<ModelInfo>>;
    async fn load_skill(&self, name: &str) -> anyhow::Result<()>;
    fn subscribe_events(&self) -> broadcast::Receiver<AgentEvent>;
    fn agent_type(&self) -> &'static str;
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum AgentType {
    #[serde(rename = "pi")]
    Pi,
    #[serde(rename = "opencode")]
    Opencode,
}

impl Default for AgentType {
    fn default() -> Self {
        AgentType::Pi
    }
}

impl std::fmt::Display for AgentType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentType::Pi => write!(f, "pi"),
            AgentType::Opencode => write!(f, "opencode"),
        }
    }
}

impl std::str::FromStr for AgentType {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "pi" => Ok(AgentType::Pi),
            "opencode" => Ok(AgentType::Opencode),
            _ => anyhow::bail!("Unknown agent type: {}", s),
        }
    }
}

pub mod opencode;
pub mod pi;

pub use opencode::OpencodeAgent;
pub use pi::PiAgent;

// NoOpAgent - 用于不需要 agent 的命令（如 /agent）
pub struct NoOpAgent;

#[async_trait]
impl AiAgent for NoOpAgent {
    async fn prompt(&self, _message: &str) -> anyhow::Result<()> { Ok(()) }
    async fn set_session_name(&self, _name: &str) -> anyhow::Result<()> { Ok(()) }
    async fn get_state(&self) -> anyhow::Result<AgentState> {
        Ok(AgentState { message_count: 0, model: None })
    }
    async fn compact(&self) -> anyhow::Result<()> { Ok(()) }
    async fn abort(&self) -> anyhow::Result<()> { Ok(()) }
    async fn clear(&self) -> anyhow::Result<()> { Ok(()) }
    async fn set_model(&self, _provider: &str, _model_id: &str) -> anyhow::Result<()> { Ok(()) }
    async fn set_thinking_level(&self, _level: &str) -> anyhow::Result<()> { Ok(()) }
    async fn get_available_models(&self) -> anyhow::Result<Vec<ModelInfo>> { Ok(vec![]) }
    async fn load_skill(&self, _name: &str) -> anyhow::Result<()> { Ok(()) }
    fn subscribe_events(&self) -> broadcast::Receiver<AgentEvent> {
        let (_, rx) = broadcast::channel(1);
        rx
    }
    fn agent_type(&self) -> &'static str { "noop" }
}
