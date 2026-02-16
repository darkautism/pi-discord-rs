use super::SlashCommand;
use async_trait::async_trait;
use serenity::all::{CommandInteraction, Context, EditInteractionResponse};
use std::sync::Arc;

use crate::agent::AiAgent;

pub struct CompactCommand;

#[async_trait]
impl SlashCommand for CompactCommand {
    fn name(&self) -> &'static str {
        "compact"
    }

    fn description(&self) -> &'static str {
        "壓縮對話歷史以節省 Token"
    }

    async fn execute(
        &self,
        ctx: &Context,
        command: &CommandInteraction,
        agent: Arc<dyn AiAgent>,
    ) -> anyhow::Result<()> {
        command.defer_ephemeral(&ctx.http).await?;

        agent.compact().await?;

        command
            .edit_response(
                &ctx.http,
                EditInteractionResponse::new().content("✅ 已壓縮對話歷史"),
            )
            .await?;

        Ok(())
    }
}
