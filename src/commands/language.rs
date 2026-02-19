use super::SlashCommand;
use async_trait::async_trait;
use serenity::all::{
    CommandInteraction, CommandOptionType, Context, CreateCommandOption, EditInteractionResponse,
};
use tracing::{error, info};

use crate::i18n::I18n;

pub struct LanguageCommand;

#[async_trait]
impl SlashCommand for LanguageCommand {
    fn name(&self) -> &'static str {
        "language"
    }

    fn description(&self, i18n: &I18n) -> String {
        i18n.get("cmd_lang_desc")
    }

    fn options(&self, i18n: &I18n) -> Vec<CreateCommandOption> {
        vec![CreateCommandOption::new(
            CommandOptionType::String,
            "lang",
            i18n.get("cmd_lang_opt_lang"),
        )
        .required(true)
        .add_string_choice("繁體中文", "zh-TW")
        .add_string_choice("English", "en")]
    }

    async fn execute(
        &self,
        ctx: &Context,
        command: &CommandInteraction,
        state: &crate::AppState,
    ) -> anyhow::Result<()> {
        command.defer_ephemeral(&ctx.http).await?;

        let lang = command
            .data
            .options
            .iter()
            .find(|o| o.name == "lang")
            .and_then(|o| o.value.as_str())
            .unwrap_or("zh-TW");

        // 1. 更新內存中的 i18n 實例
        {
            let mut i18n_lock = state.i18n.write().await;
            *i18n_lock = I18n::new(lang);
        }

        // 2. 更新 config 檔案
        if let Ok(mut config) = crate::config::Config::load().await {
            config.language = lang.to_string();
            // 注意：這裡需要實作一個 save 方法到 Config，或者直接手動寫入。
            // 為了簡化，我們先確保內存生效。
            let config_path = crate::migrate::get_config_path();
            let toml_str = toml::to_string_pretty(&config)?;
            if let Err(e) = tokio::fs::write(config_path, toml_str).await {
                error!("❌ Failed to persist language setting: {}", e);
            }
        }

        let msg = {
            let i18n = state.i18n.read().await;
            i18n.get_args("lang_switched", &[lang.to_string()])
        };

        command
            .edit_response(&ctx.http, EditInteractionResponse::new().content(msg))
            .await?;

        // 3. 關鍵：重新註冊所有 Slash Commands 以更新說明文字
        let i18n = state.i18n.read().await;
        let commands = super::get_all_commands()
            .into_iter()
            .map(|cmd| cmd.create_command(&i18n))
            .collect::<Vec<_>>();

        match serenity::all::Command::set_global_commands(&ctx.http, commands).await {
            Ok(_) => {
                info!("✅ Re-registered global commands for language: {}", lang);
                let final_msg = i18n.get_args("lang_updated", &[lang.to_string()]);
                command
                    .edit_response(&ctx.http, EditInteractionResponse::new().content(final_msg))
                    .await?;
            }
            Err(e) => {
                error!("❌ Failed to re-register commands: {}", e);
            }
        }

        Ok(())
    }
}
