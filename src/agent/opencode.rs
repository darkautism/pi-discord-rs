use super::{AgentEvent, AgentState, AiAgent, ModelInfo};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::sync::Mutex;

#[derive(Debug, Serialize, Deserialize)]
struct ChatRequest {
    messages: Vec<Message>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatStreamChunk {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    delta: Delta,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Delta {
    content: Option<String>,
    reasoning_content: Option<String>,
}

pub struct OpencodeAgent {
    client: Client,
    api_url: String,
    api_key: String,
    model: Mutex<Option<String>>,
    messages: Mutex<Vec<Message>>,
    event_tx: broadcast::Sender<AgentEvent>,
}

impl OpencodeAgent {
    pub fn new(api_url: String, api_key: String) -> Arc<Self> {
        let (event_tx, _) = broadcast::channel(1000);
        Arc::new(Self {
            client: Client::new(),
            api_url,
            api_key,
            model: Mutex::new(None),
            messages: Mutex::new(Vec::new()),
            event_tx,
        })
    }

    async fn handle_stream(&self, mut response: reqwest::Response) -> anyhow::Result<()> {
        let mut full_thinking = String::new();
        let mut full_text = String::new();

        while let Some(chunk) = response.chunk().await? {
            let s = String::from_utf8_lossy(&chunk);
            for line in s.lines() {
                if let Some(data) = line.strip_prefix("data: ") {
                    if data == "[DONE]" {
                        break;
                    }
                    if let Ok(val) = serde_json::from_str::<ChatStreamChunk>(data) {
                        for choice in val.choices {
                            if let Some(thought) = choice.delta.reasoning_content {
                                full_thinking.push_str(&thought);
                                let _ = self.event_tx.send(AgentEvent::MessageUpdate {
                                    thinking: thought,
                                    text: String::new(),
                                    is_delta: true,
                                    id: None,
                                });
                            }
                            if let Some(content) = choice.delta.content {
                                full_text.push_str(&content);
                                let _ = self.event_tx.send(AgentEvent::MessageUpdate {
                                    thinking: String::new(),
                                    text: content,
                                    is_delta: true,
                                    id: None,
                                });
                            }
                            if choice.finish_reason.is_some() {
                                let mut msgs = self.messages.lock().await;
                                msgs.push(Message {
                                    role: "assistant".into(),
                                    content: full_text.clone(),
                                });
                                let _ = self.event_tx.send(AgentEvent::AgentEnd {
                                    success: true,
                                    error: None,
                                });
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

#[async_trait]
impl AiAgent for OpencodeAgent {
    async fn prompt(&self, message: &str) -> anyhow::Result<()> {
        let mut msgs = self.messages.lock().await;
        msgs.push(Message {
            role: "user".into(),
            content: message.to_string(),
        });

        let model = self.model.lock().await.clone();
        let req = ChatRequest {
            messages: msgs.clone(),
            stream: true,
            model,
        };

        let response = self
            .client
            .post(format!("{}/chat/completions", self.api_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&req)
            .send()
            .await?;

        if !response.status().is_success() {
            let err = response.text().await?;
            return Err(anyhow::anyhow!("API Error: {}", err));
        }

        let agent = self.event_tx.clone(); // Capture sender
        let self_clone = Arc::new(OpencodeAgent {
            client: self.client.clone(),
            api_url: self.api_url.clone(),
            api_key: self.api_key.clone(),
            model: Mutex::new(None),
            messages: Mutex::new(Vec::new()),
            event_tx: agent,
        }); // Minimal clone for thread safety in stream

        tokio::spawn(async move {
            if let Err(e) = self_clone.handle_stream(response).await {
                let _ = self_clone.event_tx.send(AgentEvent::Error {
                    message: e.to_string(),
                });
            }
        });

        Ok(())
    }

    async fn set_session_name(&self, _name: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn get_state(&self) -> anyhow::Result<AgentState> {
        let msgs = self.messages.lock().await;
        Ok(AgentState {
            message_count: msgs.len() as u64,
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
        let mut msgs = self.messages.lock().await;
        msgs.clear();
        Ok(())
    }
    async fn set_model(&self, _p: &str, mid: &str) -> anyhow::Result<()> {
        let mut m = self.model.lock().await;
        *m = Some(mid.to_string());
        Ok(())
    }
    async fn set_thinking_level(&self, _l: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn get_available_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        Ok(vec![
            ModelInfo {
                provider: "opencode".into(),
                id: "deepseek-r1".into(),
                label: "DeepSeek R1".to_string(),
            },
            ModelInfo {
                provider: "opencode".into(),
                id: "gpt-4o".into(),
                label: "GPT-4o".to_string(),
            },
        ])
    }
    async fn load_skill(&self, _n: &str) -> anyhow::Result<()> {
        Ok(())
    }
    fn subscribe_events(&self) -> broadcast::Receiver<AgentEvent> {
        self.event_tx.subscribe()
    }
    fn agent_type(&self) -> &'static str {
        "opencode"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn test_opencode_event_flow() {
        let (tx, mut rx) = broadcast::channel(10);
        let _ = tx.send(AgentEvent::MessageUpdate {
            thinking: "Think".into(),
            text: "".into(),
            is_delta: true,
            id: None,
        });
        let ev = rx.recv().await.unwrap();
        if let AgentEvent::MessageUpdate { thinking, .. } = ev {
            assert_eq!(thinking, "Think");
        }
    }
}
