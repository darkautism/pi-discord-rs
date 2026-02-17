use super::SlashCommand;
use async_trait::async_trait;
use serenity::all::{CommandInteraction, Context, EditInteractionResponse};

use super::agent::ChannelConfig;
use crate::migrate;

pub struct ClearCommand;

#[async_trait]
impl SlashCommand for ClearCommand {
    fn name(&self) -> &'static str {
        "clear"
    }

    fn description(&self, i18n: &crate::i18n::I18n) -> String {
        i18n.get("cmd_clear_desc")
    }

    async fn execute(
        &self,
        ctx: &Context,
        command: &CommandInteraction,
        state: &crate::AppState,
    ) -> anyhow::Result<()> {
        command.defer_ephemeral(&ctx.http).await?;

        let channel_id_u64 = command.channel_id.get();
        let channel_id_str = channel_id_u64.to_string();
        let channel_config = crate::commands::agent::ChannelConfig::load()
            .await
            .unwrap_or_default();
        let agent_type = channel_config.get_agent_type(&channel_id_str);

        let (agent, _) = state
            .session_manager
            .get_or_create_session(channel_id_u64, agent_type, &state.backend_manager)
            .await?;

        // 1. 清除後端 session
        agent.clear().await?;

        // 2. 移除記憶體快取
        state.session_manager.remove_session(channel_id_u64).await;

        // 3. 刪除本地 session 檔案
        let agent_type = agent.agent_type();
        let session_file = migrate::get_sessions_dir(agent_type)
            .join(format!("discord-rs-{}.jsonl", channel_id_u64));

        if session_file.exists() {
            tokio::fs::remove_file(&session_file).await.ok();
        }

        // 4. 清除持久化配置中的 ID
        if let Ok(mut config) = ChannelConfig::load().await {
            if let Some(entry) = config.channels.get_mut(&channel_id_str) {
                entry.session_id = None;
                let _ = config.save().await;
            }
        }

        let i18n = state.i18n.read().await;
        let msg = i18n.get("clear_success");
        drop(i18n);

        command
            .edit_response(&ctx.http, EditInteractionResponse::new().content(msg))
            .await?;

        Ok(())
    }
}
