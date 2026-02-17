use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

#[derive(Clone, Debug)]
#[allow(dead_code)]
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

#[derive(Clone, Debug, PartialEq)]
pub enum ContentType {
    Thinking,
    Text,
    ToolCall(String), // name
    ToolOutput,
}

#[derive(Clone, Debug)]
pub struct ContentItem {
    pub type_: ContentType,
    pub content: String,
    pub id: Option<String>, // 新增 ID 支持
}

#[derive(Clone, Debug)]
pub enum AgentEvent {
    MessageUpdate {
        thinking: String,
        text: String,
        is_delta: bool,
        id: Option<String>,
    },
    ContentSync {
        items: Vec<ContentItem>,
    },
    ToolExecutionStart {
        id: String,
        name: String,
    },
    ToolExecutionUpdate {
        id: String,
        output: String,
    },
    #[allow(dead_code)]
    ToolExecutionEnd {
        id: String,
        name: String,
    },
    AgentEnd {
        success: bool,
        error: Option<String>,
    },
    #[allow(dead_code)]
    AutoRetry {
        attempt: u64,
        max: u64,
    },
    Error {
        message: String,
    },
    CommandResponse {
        id: String,
        data: serde_json::Value,
    },
}

#[async_trait]
pub trait AiAgent: Send + Sync {
    async fn prompt(&self, message: &str) -> anyhow::Result<()>;
    #[allow(dead_code)]
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
    #[serde(rename = "kilo")]
    Kilo,
}

impl Default for AgentType {
    fn default() -> Self {
        AgentType::Kilo
    } // 將 Kilo 設為預設，因為它更省資源
}

impl std::fmt::Display for AgentType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentType::Pi => write!(f, "pi"),
            AgentType::Opencode => write!(f, "opencode"),
            AgentType::Kilo => write!(f, "kilo"),
        }
    }
}

impl std::str::FromStr for AgentType {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "pi" => Ok(AgentType::Pi),
            "opencode" => Ok(AgentType::Opencode),
            "kilo" => Ok(AgentType::Kilo),
            _ => anyhow::bail!("Unknown agent type: {}", s),
        }
    }
}

pub mod manager;
pub mod kilo;
pub mod opencode;
pub mod pi;
pub use kilo::KiloAgent;
pub use opencode::OpencodeAgent;
pub use pi::PiAgent;

pub struct NoOpAgent;
#[async_trait]
impl AiAgent for NoOpAgent {
    async fn prompt(&self, _message: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn set_session_name(&self, _name: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn get_state(&self) -> anyhow::Result<AgentState> {
        Ok(AgentState {
            message_count: 0,
            model: None,
        })
    }
    async fn compact(&self) -> anyhow::Result<()> {
        Ok(())
    }
    async fn abort(&self) -> anyhow::Result<()> {
        Ok(())
    }
    async fn clear(&self) -> anyhow::Result<()> {
        Ok(())
    }
    async fn set_model(&self, _p: &str, _m: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn set_thinking_level(&self, _l: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn get_available_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        Ok(vec![])
    }
    async fn load_skill(&self, _n: &str) -> anyhow::Result<()> {
        Ok(())
    }
    fn subscribe_events(&self) -> broadcast::Receiver<AgentEvent> {
        let (_, rx) = broadcast::channel(1);
        rx
    }
    fn agent_type(&self) -> &'static str {
        "noop"
    }
}
