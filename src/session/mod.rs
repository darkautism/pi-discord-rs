use crate::agent::{AiAgent, AgentType, PiAgent, OpencodeAgent};
use crate::config::Config;
use crate::migrate;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

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
    ) -> anyhow::Result<(Arc<dyn AiAgent>, bool)> {
        // 檢查現有 session
        {
            let sessions = self.sessions.read().await;
            if let Some(session) = sessions.get(&channel_id) {
                // 檢查 agent 類型是否匹配
                if session.agent_type() == agent_type.to_string() {
                    return Ok((session.clone(), false));
                }
            }
        }

        // 創建新 session
        let session: Arc<dyn AiAgent> = match agent_type {
            AgentType::Pi => {
                let session_dir = migrate::get_sessions_dir("pi");
                std::fs::create_dir_all(&session_dir)?;
                let (pi_agent, _) = PiAgent::new(channel_id, &session_dir).await?;
                pi_agent
            }
            AgentType::Opencode => {
                let op_conf = &self.config.opencode;
                let api_url = format!("http://{}:{}", op_conf.host, op_conf.port);
                let api_key = op_conf.password.clone().unwrap_or_default();
                OpencodeAgent::new(api_url, api_key)
            }
        };

        // 儲存 session
        {
            let mut sessions = self.sessions.write().await;
            sessions.insert(channel_id, session.clone());
        }

        // 檢查是否為磁碟上的全新會話
        let is_brand_new = if let Ok(state) = session.get_state().await {
            state.message_count == 0
        } else {
            true
        };

        Ok((session, is_brand_new))
    }

    pub async fn remove_session(&self, channel_id: u64) {
        let mut sessions = self.sessions.write().await;
        sessions.remove(&channel_id);
    }

    pub async fn clear_session(&self, channel_id: u64, _agent_type: AgentType) -> anyhow::Result<()> {
        self.remove_session(channel_id).await;
        Ok(())
    }

    pub async fn has_session(&self, channel_id: u64) -> bool {
        let sessions = self.sessions.read().await;
        sessions.contains_key(&channel_id)
    }
}
