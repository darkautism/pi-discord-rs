use super::SlashCommand;
use async_trait::async_trait;
use serenity::all::{
    CommandInteraction, CommandOptionType, Context, CreateCommandOption, EditInteractionResponse,
};
use std::sync::Arc;

use crate::agent::AiAgent;
use crate::auth::AuthManager;

pub struct MentionOnlyCommand;

#[async_trait]
impl SlashCommand for MentionOnlyCommand {
    fn name(&self) -> &'static str {
        "mention_only"
    }

    fn description(&self) -> &'static str {
        "切換 Mention 模式（僅限已認證頻道）"
    }

    fn options(&self) -> Vec<CreateCommandOption> {
        vec![CreateCommandOption::new(
            CommandOptionType::Boolean,
            "enable",
            "是否啟用 Mention Only 模式",
        )
        .required(true)]
    }

    async fn execute(
        &self,
        ctx: &Context,
        command: &CommandInteraction,
        _agent: Arc<dyn AiAgent>,
    ) -> anyhow::Result<()> {
        command.defer_ephemeral(&ctx.http).await?;

        let enable = command
            .data
            .options
            .iter()
            .find(|o| o.name == "enable")
            .and_then(|o| o.value.as_bool())
            .unwrap_or(true);

        let ch_id = command.channel_id.to_string();
        let auth = AuthManager::new();

        let msg = match auth.set_mention_only(&ch_id, enable) {
            Ok(_) => format!("✅ Mention-only 模式: **{}**", if enable { "啟用" } else { "停用" }),
            Err(_) => "❌ 頻道尚未認證".to_string(),
        };

        command
            .edit_response(&ctx.http, EditInteractionResponse::new().content(msg))
            .await?;

        Ok(())
    }
}
