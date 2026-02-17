use super::SlashCommand;
use async_trait::async_trait;
use serenity::all::{
    CommandInteraction, CommandOptionType, Context, CreateCommandOption, EditInteractionResponse,
};

pub struct MentionOnlyCommand;

#[async_trait]
impl SlashCommand for MentionOnlyCommand {
    fn name(&self) -> &'static str {
        "mention_only"
    }

    fn description(&self, i18n: &crate::i18n::I18n) -> String {
        i18n.get("cmd_mention_desc")
    }

    fn options(&self, i18n: &crate::i18n::I18n) -> Vec<CreateCommandOption> {
        vec![CreateCommandOption::new(
            CommandOptionType::Boolean,
            "enable",
            i18n.get("cmd_mention_opt_enabled"),
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

        let enable = command
            .data
            .options
            .iter()
            .find(|o| o.name == "enable")
            .and_then(|o| o.value.as_bool())
            .unwrap_or(true);

        let ch_id = command.channel_id.to_string();
        let auth = state.auth.clone();

        let i18n = state.i18n.read().await;
        let msg = match auth.set_mention_only(&ch_id, enable) {
            Ok(_) => i18n.get(if enable { "mention_on" } else { "mention_off" }),
            Err(_) => i18n.get("mention_not_auth"),
        };
        drop(i18n);

        command
            .edit_response(&ctx.http, EditInteractionResponse::new().content(msg))
            .await?;

        Ok(())
    }
}
