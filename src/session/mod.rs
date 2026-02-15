use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::agent::{AgentType, AiAgent, OpencodeAgent, PiAgent};
use crate::config::Config;
use crate::migrate;

pub struct SessionManager {
    sessions: Arc<RwLock<HashMap<u64, Arc<dyn AiAgent>>>>,
    config: Arc<Config>,
}

impl SessionManager {
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            config,
        }
    }

    pub async fn get_or_create_session(
        &self,
        channel_id: u64,
        agent_type: AgentType,
    ) -> anyhow::Result<Arc<dyn AiAgent>> {
        // 檢查現有 session
        {
            let sessions = self.sessions.read().await;
            if let Some(session) = sessions.get(&channel_id) {
                // 檢查 agent 類型是否匹配
                if session.agent_type() == agent_type.to_string() {
                    return Ok(session.clone());
                }
            }
        }

        // 創建新 session
        let session: Arc<dyn AiAgent> = match agent_type {
            AgentType::Pi => {
                let session_dir = migrate::get_sessions_dir("pi");
                let (pi_agent, _) = PiAgent::new(channel_id, &session_dir).await?;
                pi_agent
            }
            AgentType::Opencode => {
                let opencode_config = crate::agent::opencode::OpencodeConfig {
                    host: self.config.opencode.host.clone(),
                    port: self.config.opencode.port,
                    password: self.config.opencode.password.clone(),
                };
                OpencodeAgent::new(channel_id, &opencode_config).await?
            }
        };

        // 儲存 session
        {
            let mut sessions = self.sessions.write().await;
            sessions.insert(channel_id, session.clone());
        }

        Ok(session)
    }

    pub async fn remove_session(&self, channel_id: u64) {
        let mut sessions = self.sessions.write().await;
        sessions.remove(&channel_id);
    }

    pub async fn clear_session(&self, channel_id: u64, agent_type: AgentType) -> anyhow::Result<()> {
        // 移除記憶體中的 session
        self.remove_session(channel_id).await;

        // 刪除本地 session 檔案
        let session_dir = migrate::get_sessions_dir(&agent_type.to_string());
        let session_file = session_dir.join(format!("discord-rs-{}.jsonl", channel_id));
        
        if session_file.exists() {
            tokio::fs::remove_file(&session_file).await.ok();
        }

        Ok(())
    }

    pub async fn has_session(&self, channel_id: u64) -> bool {
        let sessions = self.sessions.read().await;
        sessions.contains_key(&channel_id)
    }
}
