use super::SlashCommand;
use async_trait::async_trait;
use serenity::all::{
    CommandInteraction, CommandOptionType, Context, CreateCommandOption, EditInteractionResponse,
};
use std::sync::Arc;

use crate::agent::AiAgent;

pub struct ThinkingCommand;

#[async_trait]
impl SlashCommand for ThinkingCommand {
    fn name(&self) -> &'static str {
        "thinking"
    }

    fn description(&self) -> &'static str {
        "設定思考等級"
    }

    fn options(&self) -> Vec<CreateCommandOption> {
        vec![
            CreateCommandOption::new(CommandOptionType::String, "level", "思考等級")
                .required(true)
                .add_string_choice("off", "off")
                .add_string_choice("minimal", "minimal")
                .add_string_choice("low", "low")
                .add_string_choice("medium", "medium")
                .add_string_choice("high", "high")
                .add_string_choice("xhigh", "xhigh"),
        ]
    }

    async fn execute(
        &self,
        ctx: &Context,
        command: &CommandInteraction,
        agent: Arc<dyn AiAgent>,
    ) -> anyhow::Result<()> {
        command.defer_ephemeral(&ctx.http).await?;

        let level = command
            .data
            .options
            .iter()
            .find(|o| o.name == "level")
            .and_then(|o| o.value.as_str())
            .unwrap_or("medium");

        match agent.set_thinking_level(level).await {
            Ok(_) => {
                command
                    .edit_response(
                        &ctx.http,
                        EditInteractionResponse::new()
                            .content(format!("✅ 已設定思考等級: {}", level)),
                    )
                    .await?;
            }
            Err(e) => {
                command
                    .edit_response(
                        &ctx.http,
                        EditInteractionResponse::new().content(format!("❌ 設定失敗: {}", e)),
                    )
                    .await?;
            }
        }

        Ok(())
    }
}
