use super::SlashCommand;
use async_trait::async_trait;
use serenity::all::{
    CommandInteraction, CommandOptionType, Context, CreateCommandOption, EditInteractionResponse,
};
use std::sync::Arc;

use crate::agent::AiAgent;

pub struct SkillCommand;

#[async_trait]
impl SlashCommand for SkillCommand {
    fn name(&self) -> &'static str {
        "skill"
    }

    fn description(&self) -> &'static str {
        "手動載入特定的 skill"
    }

    fn options(&self) -> Vec<CreateCommandOption> {
        vec![
            CreateCommandOption::new(CommandOptionType::String, "name", "Skill 名稱")
                .required(true),
        ]
    }

    async fn execute(
        &self,
        ctx: &Context,
        command: &CommandInteraction,
        agent: Arc<dyn AiAgent>,
        _state: &crate::AppState,
    ) -> anyhow::Result<()> {
        command.defer_ephemeral(&ctx.http).await?;

        let name = command
            .data
            .options
            .iter()
            .find(|o| o.name == "name")
            .and_then(|o| o.value.as_str())
            .unwrap_or("");

        match agent.load_skill(name).await {
            Ok(_) => {
                command
                    .edit_response(
                        &ctx.http,
                        EditInteractionResponse::new()
                            .content(format!("✅ 正在載入 skill: {}", name)),
                    )
                    .await?;
            }
            Err(e) => {
                command
                    .edit_response(
                        &ctx.http,
                        EditInteractionResponse::new()
                            .content(format!("❌ 載入 skill 失敗: {}", e)),
                    )
                    .await?;
            }
        }

        Ok(())
    }
}
