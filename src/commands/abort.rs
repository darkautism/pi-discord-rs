use super::SlashCommand;
use async_trait::async_trait;
use serenity::all::{CommandInteraction, Context, EditInteractionResponse};

pub struct AbortCommand;

#[async_trait]
impl SlashCommand for AbortCommand {
    fn name(&self) -> &'static str {
        "abort"
    }

    fn description(&self, i18n: &crate::i18n::I18n) -> String {
        i18n.get("cmd_abort_desc")
    }

    async fn execute(
        &self,
        ctx: &Context,
        command: &CommandInteraction,
        state: &crate::AppState,
    ) -> anyhow::Result<()> {
        command.defer_ephemeral(&ctx.http).await?;

        let channel_id_str = command.channel_id.to_string();
        let channel_config = crate::commands::agent::ChannelConfig::load()
            .await
            .unwrap_or_default();
        let agent_type = channel_config.get_agent_type(&channel_id_str);

        let (agent, _) = state
            .session_manager
            .get_or_create_session(command.channel_id.get(), agent_type, &state.backend_manager)
            .await?;

        agent.abort().await?;

        let i18n = state.i18n.read().await;
        let msg = i18n.get("abort_success");
        drop(i18n);

        command
            .edit_response(&ctx.http, EditInteractionResponse::new().content(msg))
            .await?;

        Ok(())
    }
}
