use super::SlashCommand;
use async_trait::async_trait;
use serenity::all::{CommandInteraction, Context, EditInteractionResponse};
use std::sync::Arc;

use crate::agent::AiAgent;
use crate::migrate;

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
    ) -> anyhow::Result<()> {
        command.defer_ephemeral(&ctx.http).await?;

        let channel_id = command.channel_id.get();

        // 清除 agent session
        agent.clear().await?;

        // 刪除本地 session 檔案
        let agent_type = agent.agent_type();
        let session_file =
            migrate::get_sessions_dir(agent_type).join(format!("discord-rs-{}.jsonl", channel_id));

        if session_file.exists() {
            tokio::fs::remove_file(&session_file).await.ok();
        }

        command
            .edit_response(
                &ctx.http,
                EditInteractionResponse::new().content("✅ 已清除 session"),
            )
            .await?;

        Ok(())
    }
}
