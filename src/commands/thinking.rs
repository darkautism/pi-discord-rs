use super::SlashCommand;
use async_trait::async_trait;
use serenity::all::{
    CommandInteraction, CommandOptionType, Context, CreateCommandOption, EditInteractionResponse,
};


pub struct ThinkingCommand;

#[async_trait]
impl SlashCommand for ThinkingCommand {
    fn name(&self) -> &'static str {
        "thinking"
    }

    fn description(&self, i18n: &crate::i18n::I18n) -> String {
        i18n.get("cmd_thinking_desc")
    }

    fn options(&self, i18n: &crate::i18n::I18n) -> Vec<CreateCommandOption> {
        vec![CreateCommandOption::new(
            CommandOptionType::String,
            "level",
            i18n.get("cmd_thinking_opt_level"),
        )
        .required(true)
        .add_string_choice("off", "off")
        .add_string_choice("minimal", "minimal")
        .add_string_choice("low", "low")
        .add_string_choice("medium", "medium")
        .add_string_choice("high", "high")
        .add_string_choice("xhigh", "xhigh")]
    }

    async fn execute(
        &self,
        ctx: &Context,
        command: &CommandInteraction,
        state: &crate::AppState,
    ) -> anyhow::Result<()> {
        command.defer_ephemeral(&ctx.http).await?;

        let level = command
            .data
            .options
            .iter()
            .find(|o| o.name == "level")
            .and_then(|o| o.value.as_str())
            .unwrap_or("medium");

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

        let i18n = state.i18n.read().await;
        match agent.set_thinking_level(level).await {
            Ok(_) => {
                let msg = i18n.get_args("thinking_set", &[level.to_string()]);
                command
                    .edit_response(&ctx.http, EditInteractionResponse::new().content(msg))
                    .await?;
            }
            Err(e) => {
                let msg = i18n.get_args("thinking_failed", &[e.to_string()]);
                command
                    .edit_response(&ctx.http, EditInteractionResponse::new().content(msg))
                    .await?;
            }
        }
        drop(i18n);

        Ok(())
    }
}
