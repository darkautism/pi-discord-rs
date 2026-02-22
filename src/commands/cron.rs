use async_trait::async_trait;
use serenity::all::{
    ActionRowComponent, CommandInteraction, Context, CreateActionRow, CreateInputText,
    CreateInteractionResponse, CreateModal, CreateSelectMenu, CreateSelectMenuKind,
    CreateSelectMenuOption, EditInteractionResponse, InputTextStyle, ModalInteraction,
};
use uuid::Uuid;

use crate::commands::SlashCommand;
use crate::cron::manager::CronJobInfo;
use crate::i18n::I18n;

pub struct CronCommand;

fn normalize_freq(freq: &str) -> String {
    let freq_parts: Vec<&str> = freq.split_whitespace().collect();
    match freq_parts.len() {
        1 => format!("{} * *", freq),
        2 => format!("{} *", freq),
        3 => freq.to_string(),
        _ => "* * *".to_string(),
    }
}

fn build_cron_expr(minute: &str, hour: &str, freq: &str) -> String {
    format!("0 {} {} {}", minute, hour, normalize_freq(freq))
}

fn prompt_preview(prompt: &str, max_chars: usize) -> String {
    if prompt.len() <= max_chars {
        return prompt.to_string();
    }
    if max_chars <= 3 {
        return "...".to_string();
    }
    let mut end = max_chars - 3;
    while !prompt.is_char_boundary(end) && end > 0 {
        end -= 1;
    }
    format!("{}...", &prompt[..end])
}

pub async fn handle_modal_submit(
    ctx: &Context,
    interaction: &ModalInteraction,
    state: &crate::AppState,
) -> anyhow::Result<()> {
    interaction.defer_ephemeral(&ctx.http).await?;

    let mut minute = String::from("*");
    let mut hour = String::from("*");
    let mut freq = String::from("* * *");
    let mut prompt = String::new();

    for row in &interaction.data.components {
        for component in &row.components {
            if let ActionRowComponent::InputText(text) = component {
                match text.custom_id.as_str() {
                    "cron_minute" => minute = text.value.clone().unwrap_or_else(|| "*".into()),
                    "cron_hour" => hour = text.value.clone().unwrap_or_else(|| "*".into()),
                    "cron_freq" => freq = text.value.clone().unwrap_or_else(|| "* * *".into()),
                    "cron_prompt" => prompt = text.value.clone().unwrap_or_default(),
                    _ => {}
                }
            }
        }
    }

    // 構建 6 位 Cron: 秒(0) 分 時 日 月 週
    let cron_expr = build_cron_expr(&minute, &hour, &freq);

    // 驗證並翻譯成「人話」
    let i18n = state.i18n.read().await;
    let description = match cron_descriptor::cronparser::cron_expression_descriptor::get_description(
        cron_descriptor::cronparser::DescriptionTypeEnum::FULL,
        &cron_expr,
        &cron_descriptor::cronparser::Options::options(),
        "en", // 目前庫限制較多，先用 en
    ) {
        Ok(desc) => desc,
        Err(_) => {
            interaction
                .edit_response(
                    &ctx.http,
                    EditInteractionResponse::new().content(i18n.get("cron_invalid")),
                )
                .await?;
            return Ok(());
        }
    };

    let job_id = Uuid::new_v4();
    let info = CronJobInfo {
        id: job_id,
        scheduler_id: None,
        channel_id: interaction.channel_id.get(),
        cron_expr,
        prompt: prompt.to_string(),
        creator_id: interaction.user.id.get(),
        description: description.clone(),
    };

    state.cron_manager.add_job(info).await?;

    interaction
        .edit_response(
            &ctx.http,
            EditInteractionResponse::new().content(i18n.get_args("cron_success", &[description])),
        )
        .await?;

    Ok(())
}

pub async fn handle_delete_select(
    ctx: &Context,
    interaction: &serenity::all::ComponentInteraction,
    state: &crate::AppState,
) -> anyhow::Result<()> {
    interaction.defer_ephemeral(&ctx.http).await?;

    let i18n = state.i18n.read().await;

    if let serenity::all::ComponentInteractionDataKind::StringSelect { values } =
        &interaction.data.kind
    {
        if let Some(uuid_str) = values.first() {
            if let Ok(id) = Uuid::parse_str(uuid_str) {
                state.cron_manager.remove_job(id).await?;

                // 核心修復：刪除完後，傳入空 components 陣列以移除下拉選單
                interaction
                    .edit_response(
                        &ctx.http,
                        EditInteractionResponse::new()
                            .content(i18n.get_args("cron_deleted", &[uuid_str.to_string()]))
                            .components(vec![]),
                    )
                    .await?;
            }
        }
    }
    Ok(())
}

#[async_trait]
impl SlashCommand for CronCommand {
    fn name(&self) -> &'static str {
        "cron"
    }

    fn description(&self, i18n: &I18n) -> String {
        i18n.get("cmd_cron_desc")
    }

    async fn execute(
        &self,
        ctx: &Context,
        command: &CommandInteraction,
        state: &crate::AppState,
    ) -> anyhow::Result<()> {
        let i18n = state.i18n.read().await;

        let modal = CreateModal::new("cron_setup", i18n.get("cron_modal_title")).components(vec![
            CreateActionRow::InputText(
                CreateInputText::new(
                    InputTextStyle::Short,
                    i18n.get("cron_field_minute"),
                    "cron_minute",
                )
                .placeholder(i18n.get("cron_field_minute_hint"))
                .value("0")
                .required(true),
            ),
            CreateActionRow::InputText(
                CreateInputText::new(
                    InputTextStyle::Short,
                    i18n.get("cron_field_hour"),
                    "cron_hour",
                )
                .placeholder(i18n.get("cron_field_hour_hint"))
                .value("8")
                .required(true),
            ),
            CreateActionRow::InputText(
                CreateInputText::new(
                    InputTextStyle::Short,
                    i18n.get("cron_field_freq"),
                    "cron_freq",
                )
                .placeholder(i18n.get("cron_field_freq_hint"))
                .value("*")
                .required(true),
            ),
            CreateActionRow::InputText(
                CreateInputText::new(
                    InputTextStyle::Paragraph,
                    i18n.get("cron_field_prompt"),
                    "cron_prompt",
                )
                .placeholder(i18n.get("cron_field_prompt_hint"))
                .required(true),
            ),
        ]);

        command
            .create_response(&ctx.http, CreateInteractionResponse::Modal(modal))
            .await?;

        Ok(())
    }
}

pub struct CronListCommand;

#[async_trait]
impl SlashCommand for CronListCommand {
    fn name(&self) -> &'static str {
        "cron_list"
    }

    fn description(&self, i18n: &I18n) -> String {
        i18n.get("cmd_cron_list_desc")
    }

    async fn execute(
        &self,
        ctx: &Context,
        command: &CommandInteraction,
        state: &crate::AppState,
    ) -> anyhow::Result<()> {
        command.defer_ephemeral(&ctx.http).await?;

        let channel_id = command.channel_id.get();
        let jobs = state.cron_manager.get_jobs_for_channel(channel_id).await;

        let i18n = state.i18n.read().await;

        if jobs.is_empty() {
            command
                .edit_response(
                    &ctx.http,
                    EditInteractionResponse::new().content(i18n.get("cron_list_empty")),
                )
                .await?;
            return Ok(());
        }

        let mut content = format!("### {}\n", i18n.get("cron_list_title"));
        let mut options = Vec::new();

        for job in jobs {
            content.push_str(&format!(
                "- **{}**: `{}`\n  > {}\n",
                job.cron_expr, job.description, job.prompt
            ));

            options.push(
                CreateSelectMenuOption::new(
                    format!("{}: {}", job.cron_expr, job.description),
                    job.id.to_string(),
                )
                .description(prompt_preview(&job.prompt, 50)),
            );
        }

        let select_menu = CreateSelectMenu::new(
            "cron_delete_select",
            CreateSelectMenuKind::String { options },
        )
        .placeholder(i18n.get("cron_delete_placeholder"))
        .min_values(1)
        .max_values(1);

        command
            .edit_response(
                &ctx.http,
                EditInteractionResponse::new()
                    .content(content)
                    .components(vec![CreateActionRow::SelectMenu(select_menu)]),
            )
            .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{build_cron_expr, normalize_freq, prompt_preview};

    #[test]
    fn test_normalize_freq_supports_1_2_3_parts() {
        assert_eq!(normalize_freq("*"), "* * *");
        assert_eq!(normalize_freq("* 1"), "* 1 *");
        assert_eq!(normalize_freq("* * 1"), "* * 1");
        assert_eq!(normalize_freq("* * * *"), "* * *");
    }

    #[test]
    fn test_build_cron_expr_uses_6_field_format() {
        assert_eq!(build_cron_expr("0", "8", "*"), "0 0 8 * * *");
        assert_eq!(build_cron_expr("15", "9", "* * 1"), "0 15 9 * * 1");
    }

    #[test]
    fn test_prompt_preview_truncates_on_char_boundary() {
        let s = "這是一段很長的中文內容，會被安全截斷";
        let out = prompt_preview(s, 14);
        assert!(out.ends_with("..."));
        assert!(out.len() <= 17);
    }

    #[test]
    fn test_prompt_preview_short_string_unchanged() {
        assert_eq!(prompt_preview("hello", 50), "hello");
    }
}
