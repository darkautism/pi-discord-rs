use super::SlashCommand;
use async_trait::async_trait;
use serenity::all::{
    CommandInteraction, Context, CreateActionRow, CreateSelectMenu, CreateSelectMenuKind,
    CreateSelectMenuOption, EditInteractionResponse,
};
use std::sync::Arc;

use crate::agent::AiAgent;
use tracing::{error, info};

pub struct ModelCommand;

#[async_trait]
impl SlashCommand for ModelCommand {
    fn name(&self) -> &'static str {
        "model"
    }

    fn description(&self, i18n: &crate::i18n::I18n) -> String {
        i18n.get("cmd_model_desc")
    }

    // 不使用 options，改用 Select Menu
    fn options(&self, _i18n: &crate::i18n::I18n) -> Vec<serenity::all::CreateCommandOption> {
        vec![]
    }

    async fn execute(
        &self,
        ctx: &Context,
        command: &CommandInteraction,
        state: &crate::AppState,
    ) -> anyhow::Result<()> {
        // 先 defer，避免 3 秒超時
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

        let i18n = state.i18n.read().await;

        // 獲取可用模型列表
        let models = match agent.get_available_models().await {
            Ok(m) => {
                info!("Fetched {} models for /model command", m.len());
                m
            }
            Err(e) => {
                error!("Failed to fetch models: {}", e);
                command
                    .edit_response(
                        &ctx.http,
                        EditInteractionResponse::new()
                            .content(i18n.get_args("model_fetch_failed", &[e.to_string()])),
                    )
                    .await?;
                return Ok(());
            }
        };

        if models.is_empty() {
            command
                .edit_response(
                    &ctx.http,
                    EditInteractionResponse::new().content(i18n.get("model_no_available")),
                )
                .await?;
            return Ok(());
        }

        // 創建 Select Menu 選項，並分組處理（Discord 限制每組 25 個）
        let mut action_rows = Vec::new();

        // 限制最多 125 個模型 (5 rows * 25 options)
        let total_models = models.len().min(125);
        let models_slice = &models[..total_models];

        for (idx, chunk) in models_slice.chunks(25).enumerate() {
            let select_options: Vec<CreateSelectMenuOption> = chunk
                .iter()
                .map(|m| {
                    // 使用 | 作為定界符，避免與 ID 內部的 / 衝突
                    let value = format!("{}|{}", m.provider, m.id);
                    CreateSelectMenuOption::new(&m.label, value)
                        .description(format!("Provider: {}", m.provider))
                })
                .collect();

            let select_menu = CreateSelectMenu::new(
                format!("model_select_{}", idx), // 雖然 ID 變了，但 handle_model_select 也要改
                CreateSelectMenuKind::String {
                    options: select_options,
                },
            )
            .placeholder(i18n.get_args("model_placeholder", &[(idx + 1).to_string()]))
            .min_values(1)
            .max_values(1);

            action_rows.push(CreateActionRow::SelectMenu(select_menu));
        }

        // 發送帶有多個 Select Menu 的響應
        match command
            .edit_response(
                &ctx.http,
                EditInteractionResponse::new()
                    .content(i18n.get_args("model_fetched", &[total_models.to_string()]))
                    .components(action_rows),
            )
            .await
        {
            Ok(_) => info!("Successfully sent model select menu(s)"),
            Err(e) => error!("Failed to send model select menu: {}", e),
        }

        Ok(())
    }
}

// 處理模型選擇
pub async fn handle_model_select(
    ctx: &Context,
    interaction: &serenity::all::ComponentInteraction,
    agent: Arc<dyn AiAgent>,
    state: &crate::AppState,
) -> anyhow::Result<()> {
    // 先 defer，避免 3 秒超時
    interaction.defer_ephemeral(&ctx.http).await?;

    let i18n = state.i18n.read().await;

    if let serenity::all::ComponentInteractionDataKind::StringSelect { values } =
        &interaction.data.kind
    {
        if let Some(composite_id) = values.first() {
            // 使用 | 分解
            if let Some((provider, model)) = composite_id.split_once('|') {
                match agent.set_model(provider, model).await {
                    Ok(_) => {
                        interaction
                            .edit_response(
                                &ctx.http,
                                EditInteractionResponse::new()
                                    .content(
                                        i18n.get_args(
                                            "model_switched",
                                            &[composite_id.to_string()],
                                        ),
                                    )
                                    .components(vec![]), // 移除 Select Menu
                            )
                            .await?;
                    }
                    Err(e) => {
                        interaction
                            .edit_response(
                                &ctx.http,
                                EditInteractionResponse::new()
                                    .content(i18n.get_args("model_failed", &[e.to_string()]))
                                    .components(vec![]),
                            )
                            .await?;
                    }
                }
            } else {
                interaction
                    .edit_response(
                        &ctx.http,
                        EditInteractionResponse::new()
                            .content(i18n.get("model_invalid"))
                            .components(vec![]),
                    )
                    .await?;
            }
        }
    }
    Ok(())
}
