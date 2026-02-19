use super::SlashCommand;
use async_trait::async_trait;
use serenity::all::{
    CommandInteraction, CommandOptionType, Context, CreateCommandOption, EditInteractionResponse,
};

pub struct SkillCommand;

#[async_trait]
impl SlashCommand for SkillCommand {
    fn name(&self) -> &'static str {
        "skill"
    }

    fn description(&self, i18n: &crate::i18n::I18n) -> String {
        i18n.get("cmd_skill_desc")
    }

    fn options(&self, i18n: &crate::i18n::I18n) -> Vec<CreateCommandOption> {
        vec![CreateCommandOption::new(
            CommandOptionType::String,
            "name",
            i18n.get("cmd_skill_opt_name"),
        )
        .required(true)]
    }

    async fn execute(
        &self,
        ctx: &Context,
        command: &CommandInteraction,
        state: &crate::AppState,
    ) -> anyhow::Result<()> {
        command.defer_ephemeral(&ctx.http).await?;

        let name = command
            .data
            .options
            .iter()
            .find(|o| o.name == "name")
            .and_then(|o| o.value.as_str())
            .unwrap_or("");

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
        match agent.load_skill(name).await {
            Ok(_) => {
                let msg = i18n.get_args("skill_loading", &[name.to_string()]);
                command
                    .edit_response(&ctx.http, EditInteractionResponse::new().content(msg))
                    .await?;
            }
            Err(e) => {
                let msg = i18n.get_args("skill_failed", &[e.to_string()]);
                command
                    .edit_response(&ctx.http, EditInteractionResponse::new().content(msg))
                    .await?;
            }
        }
        drop(i18n);

        Ok(())
    }
}
