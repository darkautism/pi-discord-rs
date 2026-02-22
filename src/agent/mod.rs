use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::Path;
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
pub struct UploadedFile {
    pub id: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub local_path: String,
    pub source_url: String,
}

impl UploadedFile {
    pub fn is_image(&self) -> bool {
        self.mime.starts_with("image/")
    }

    pub fn display_name(&self) -> String {
        if self.name.is_empty() {
            Path::new(&self.local_path)
                .file_name()
                .and_then(|v| v.to_str())
                .unwrap_or("unknown")
                .to_string()
        } else {
            self.name.clone()
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct UserInput {
    pub text: String,
    pub files: Vec<UploadedFile>,
}

impl UserInput {
    pub fn new_text(text: String) -> Self {
        Self {
            text,
            files: Vec::new(),
        }
    }

    pub fn to_fallback_prompt(&self) -> String {
        if self.files.is_empty() {
            return self.text.clone();
        }

        let mut file_lines = Vec::new();
        for (idx, file) in self.files.iter().enumerate() {
            file_lines.push(format!(
                "{}. {} | mime={} | size={}B | local_path={}",
                idx + 1,
                file.display_name(),
                file.mime,
                file.size,
                file.local_path
            ));
        }

        format!(
            "{}\n\n[Uploaded Files]\n{}\n\nUse these file paths if your tools can read local files.",
            self.text,
            file_lines.join("\n")
        )
    }
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
    async fn prompt_with_input(&self, input: &UserInput) -> anyhow::Result<()> {
        self.prompt(&input.to_fallback_prompt()).await
    }
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub enum AgentType {
    #[serde(rename = "pi")]
    Pi,
    #[serde(rename = "opencode")]
    Opencode,
    #[serde(rename = "copilot")]
    Copilot,
    #[serde(rename = "kilo")]
    #[default]
    Kilo,
}

impl std::fmt::Display for AgentType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentType::Pi => write!(f, "pi"),
            AgentType::Opencode => write!(f, "opencode"),
            AgentType::Copilot => write!(f, "copilot"),
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
            "copilot" => Ok(AgentType::Copilot),
            "kilo" => Ok(AgentType::Kilo),
            _ => anyhow::bail!("Unknown agent type: {}", s),
        }
    }
}

pub mod copilot;
pub mod kilo;
pub mod manager;
pub mod opencode;
pub mod pi;
pub mod runtime;
pub use copilot::CopilotAgent;
pub use kilo::KiloAgent;
pub use opencode::OpencodeAgent;
pub use pi::PiAgent;

#[cfg(test)]
pub struct MockAgent {
    pub tx: tokio::sync::broadcast::Sender<AgentEvent>,
}

#[cfg(test)]
impl MockAgent {
    pub fn new() -> Self {
        let (tx, _) = tokio::sync::broadcast::channel(100);
        Self { tx }
    }
}

#[cfg(test)]
#[async_trait]
impl AiAgent for MockAgent {
    async fn prompt(&self, _message: &str) -> anyhow::Result<()> {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(AgentEvent::MessageUpdate {
                thinking: "Thinking...".into(),
                text: "Mock Response".into(),
                is_delta: false,
                id: Some("test-1".into()),
            });
            let _ = tx.send(AgentEvent::AgentEnd {
                success: true,
                error: None,
            });
        });
        Ok(())
    }
    async fn set_session_name(&self, _name: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn get_state(&self) -> anyhow::Result<AgentState> {
        Ok(AgentState {
            message_count: 1,
            model: Some("mock".into()),
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
        self.tx.subscribe()
    }
    fn agent_type(&self) -> &'static str {
        "mock"
    }
}

#[cfg(test)]
mod tests {
    use super::{UploadedFile, UserInput};

    #[test]
    fn test_uploaded_file_display_name_fallback_to_path() {
        let file = UploadedFile {
            id: "1".to_string(),
            name: String::new(),
            mime: "text/plain".to_string(),
            size: 10,
            local_path: "/tmp/demo/a.txt".to_string(),
            source_url: "https://example.com/a.txt".to_string(),
        };
        assert_eq!(file.display_name(), "a.txt");
    }

    #[test]
    fn test_user_input_fallback_prompt_includes_files_section() {
        let input = UserInput {
            text: "Please analyze files".to_string(),
            files: vec![UploadedFile {
                id: "f1".to_string(),
                name: "image.png".to_string(),
                mime: "image/png".to_string(),
                size: 1234,
                local_path: "/tmp/uploads/image.png".to_string(),
                source_url: "https://cdn.discordapp.com/x".to_string(),
            }],
        };

        let rendered = input.to_fallback_prompt();
        assert!(rendered.contains("[Uploaded Files]"));
        assert!(rendered.contains("image.png"));
        assert!(rendered.contains("mime=image/png"));
        assert!(rendered.contains("local_path=/tmp/uploads/image.png"));
    }
}
