use super::SlashCommand;
use async_trait::async_trait;
use serenity::all::{CommandInteraction, Context, EditInteractionResponse};
use std::sync::Arc;

use crate::agent::AiAgent;
use crate::migrate;
use super::agent::ChannelConfig;

pub struct ClearCommand;

#[async_trait]
impl SlashCommand for ClearCommand {
    fn name(&self) -> &'static str {
        "clear"
    }

    fn description(&self) -> &'static str {
        "硬清除當前對話進程並刪除歷史存檔"
    }

    async fn execute(
        &self,
        ctx: &Context,
        command: &CommandInteraction,
        agent: Arc<dyn AiAgent>,
        state: &crate::AppState,
    ) -> anyhow::Result<()> {
        command.defer_ephemeral(&ctx.http).await?;

        let channel_id = command.channel_id.get();

        // 1. 清除後端 session
        agent.clear().await?;

        // 2. 移除記憶體快取
        state.session_manager.remove_session(channel_id).await;

        // 3. 刪除本地 session 檔案
        let agent_type = agent.agent_type();
        let session_file =
            migrate::get_sessions_dir(agent_type).join(format!("discord-rs-{}.jsonl", channel_id));

        if session_file.exists() {
            tokio::fs::remove_file(&session_file).await.ok();
        }

        // 4. 清除持久化配置中的 ID
        if let Ok(mut config) = ChannelConfig::load().await {
            if let Some(entry) = config.channels.get_mut(&channel_id.to_string()) {
                entry.session_id = None;
                let _ = config.save().await;
            }
        }

        command
            .edit_response(
                &ctx.http,
                EditInteractionResponse::new().content("✅ 已徹底清除會話狀態"),
            )
            .await?;

        Ok(())
    }
}
