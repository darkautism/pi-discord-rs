use crate::agent::{AgentType, AiAgent, KiloAgent, OpencodeAgent, PiAgent};
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
            AgentType::Kilo => {
                let channel_id_str = channel_id.to_string();
                let channel_config = crate::commands::agent::ChannelConfig::load()
                    .await
                    .unwrap_or_default();
                let entry = channel_config.channels.get(&channel_id_str);

                let existing_sid = entry.and_then(|e| e.kilo_session_id.clone());
                let model_opt = entry.and_then(|e| {
                    if let (Some(p), Some(m)) = (&e.model_provider, &e.model_id) {
                        Some((p.clone(), m.clone()))
                    } else {
                        None
                    }
                });

                let api_url = "http://127.0.0.1:3333".to_string();
                let agent = KiloAgent::new(channel_id, api_url, existing_sid, model_opt).await?;

                // 如果是新創建的，更新配置儲存
                let mut channel_config = crate::commands::agent::ChannelConfig::load()
                    .await
                    .unwrap_or_default();
                let entry = channel_config
                    .channels
                    .entry(channel_id_str)
                    .or_insert_with(|| crate::commands::agent::ChannelEntry {
                        agent_type: AgentType::Kilo,
                        authorized_at: chrono::Utc::now().to_rfc3339(),
                        mention_only: true,
                        kilo_session_id: None,
                        model_provider: None,
                        model_id: None,
                    });
                entry.kilo_session_id = Some(agent.session_id.clone());
                channel_config.save().await?;

                agent
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

}
