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
        backend_manager: &crate::agent::manager::BackendManager,
    ) -> anyhow::Result<(Arc<dyn AiAgent>, bool)> {
        {
            let sessions = self.sessions.read().await;
            if let Some(session) = sessions.get(&channel_id) {
                if session.agent_type() == agent_type.to_string() {
                    return Ok((session.clone(), false));
                }
            }
        }

        let channel_id_str = channel_id.to_string();
        let channel_config = crate::commands::agent::ChannelConfig::load()
            .await
            .unwrap_or_default();
        let entry = channel_config.channels.get(&channel_id_str);

        let model_opt = entry.and_then(|e| {
            if let (Some(p), Some(m)) = (&e.model_provider, &e.model_id) {
                Some((p.clone(), m.clone()))
            } else {
                None
            }
        });

        let existing_sid = entry.and_then(|e| e.session_id.clone());

        let session: Arc<dyn AiAgent> = match agent_type {
            AgentType::Pi => {
                let session_dir = migrate::get_sessions_dir("pi");
                std::fs::create_dir_all(&session_dir)?;
                let (pi_agent, _) = PiAgent::new(channel_id, &session_dir).await?;
                pi_agent
            }
            AgentType::Opencode => {
                let port = backend_manager.ensure_backend(&AgentType::Opencode).await?;
                let api_url = format!("http://127.0.0.1:{}", port);
                let api_key = self.config.opencode.password.clone().unwrap_or_default();

                let agent = OpencodeAgent::new(
                    channel_id,
                    api_url,
                    api_key,
                    existing_sid,
                    model_opt,
                    "opencode",
                )
                .await?;

                self.persist_sid(channel_id, AgentType::Opencode, agent.session_id.clone())
                    .await?;
                agent
            }
            AgentType::Kilo => {
                let port = backend_manager.ensure_backend(&AgentType::Kilo).await?;
                let api_url = format!("http://127.0.0.1:{}", port);

                let agent = KiloAgent::new(channel_id, api_url, existing_sid, model_opt).await?;

                self.persist_sid(channel_id, AgentType::Kilo, agent.session_id())
                    .await?;
                agent
            }
        };

        {
            let mut sessions = self.sessions.write().await;
            sessions.insert(channel_id, session.clone());
        }

        let is_brand_new = if let Ok(state) = session.get_state().await {
            state.message_count == 0
        } else {
            true
        };

        Ok((session, is_brand_new))
    }

    async fn persist_sid(
        &self,
        channel_id: u64,
        agent_type: AgentType,
        sid: String,
    ) -> anyhow::Result<()> {
        let channel_id_str = channel_id.to_string();
        let mut channel_config = crate::commands::agent::ChannelConfig::load()
            .await
            .unwrap_or_default();

        let entry = channel_config
            .channels
            .entry(channel_id_str)
            .or_insert_with(|| crate::commands::agent::ChannelEntry {
                agent_type: agent_type.clone(),
                authorized_at: chrono::Utc::now().to_rfc3339(),
                mention_only: true,
                session_id: None,
                model_provider: None,
                model_id: None,
            });

        entry.session_id = Some(sid);
        channel_config.save().await?;
        Ok(())
    }

    pub async fn remove_session(&self, channel_id: u64) {
        let mut sessions = self.sessions.write().await;
        sessions.remove(&channel_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AiAgent, MockAgent};

    #[tokio::test]
    async fn test_remove_session_clears_cached_agent() {
        let config = Arc::new(Config::default());
        let manager = SessionManager::new(config);
        let channel_id = 42_u64;
        let mock_agent: Arc<dyn AiAgent> = Arc::new(MockAgent::new());

        {
            let mut sessions = manager.sessions.write().await;
            sessions.insert(channel_id, mock_agent);
            assert!(sessions.contains_key(&channel_id));
        }

        manager.remove_session(channel_id).await;

        let sessions = manager.sessions.read().await;
        assert!(!sessions.contains_key(&channel_id));
    }
}
