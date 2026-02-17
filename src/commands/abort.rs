use super::SlashCommand;
use async_trait::async_trait;
use serenity::all::{CommandInteraction, Context, EditInteractionResponse};
use std::sync::Arc;

use crate::agent::AiAgent;

pub struct AbortCommand;

#[async_trait]
impl SlashCommand for AbortCommand {
    fn name(&self) -> &'static str {
        "abort"
    }

    fn description(&self) -> &'static str {
        "立即中斷當前正在生成的回答"
    }

    async fn execute(
        &self,
        ctx: &Context,
        command: &CommandInteraction,
        agent: Arc<dyn AiAgent>,
        _state: &crate::AppState,
    ) -> anyhow::Result<()> {
        command.defer_ephemeral(&ctx.http).await?;

        agent.abort().await?;

        command
            .edit_response(
                &ctx.http,
                EditInteractionResponse::new().content("✅ 已發送中斷信號"),
            )
            .await?;

        Ok(())
    }
}
