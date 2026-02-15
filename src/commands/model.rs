use super::SlashCommand;
use async_trait::async_trait;
use serenity::all::{
    CommandInteraction, Context, CreateActionRow, CreateSelectMenu, CreateSelectMenuKind,
    CreateSelectMenuOption, EditInteractionResponse,
};
use std::sync::Arc;

use crate::agent::AiAgent;

pub struct ModelCommand;

#[async_trait]
impl SlashCommand for ModelCommand {
    fn name(&self) -> &'static str {
        "model"
    }

    fn description(&self) -> &'static str {
        "切換當前頻道使用的模型"
    }

    // 不使用 options，改用 Select Menu
    fn options(&self) -> Vec<serenity::all::CreateCommandOption> {
        vec![]
    }

    async fn execute(
        &self,
        ctx: &Context,
        command: &CommandInteraction,
        agent: Arc<dyn AiAgent>,
    ) -> anyhow::Result<()> {
        // 先 defer，避免 3 秒超時
        command.defer_ephemeral(&ctx.http).await?;

        // 獲取可用模型列表
        let models = match agent.get_available_models().await {
            Ok(m) => m,
            Err(e) => {
                command
                    .edit_response(
                        &ctx.http,
                        EditInteractionResponse::new()
                            .content(format!("❌ 無法獲取模型列表: {}", e)),
                    )
                    .await?;
                return Ok(());
            }
        };

        if models.is_empty() {
            command
                .edit_response(
                    &ctx.http,
                    EditInteractionResponse::new().content("❌ 目前沒有可用的模型"),
                )
                .await?;
            return Ok(());
        }

        // 創建 Select Menu 選項
        let select_options: Vec<CreateSelectMenuOption> = models
            .iter()
            .map(|m| {
                CreateSelectMenuOption::new(&m.label, format!("{}/{}", m.provider, m.id))
                    .description(format!("Provider: {}", m.provider))
            })
            .collect();

        // 創建 Select Menu
        let select_menu = CreateSelectMenu::new(
            "model_select",
            CreateSelectMenuKind::String {
                options: select_options,
            },
        )
        .placeholder("選擇要切換的模型")
        .min_values(1)
        .max_values(1);

        // 發送帶有 Select Menu 的響應
        command
            .edit_response(
                &ctx.http,
                EditInteractionResponse::new()
                    .content("🤖 請選擇要使用的模型：")
                    .components(vec![CreateActionRow::SelectMenu(select_menu)]),
            )
            .await?;

        Ok(())
    }
}

// 處理模型選擇
pub async fn handle_model_select(
    ctx: &Context,
    interaction: &serenity::all::ComponentInteraction,
    agent: Arc<dyn AiAgent>,
) -> anyhow::Result<()> {
    // 先 defer，避免 3 秒超時
    interaction.defer_ephemeral(&ctx.http).await?;

    if let serenity::all::ComponentInteractionDataKind::StringSelect { values } =
        &interaction.data.kind
    {
        if let Some(model_id) = values.first() {
            // 解析 provider/model
            if let Some((provider, model)) = model_id.split_once('/') {
                match agent.set_model(provider, model).await {
                    Ok(_) => {
                        interaction
                            .edit_response(
                                &ctx.http,
                                EditInteractionResponse::new()
                                    .content(format!("✅ 已切換至模型: {}", model_id))
                                    .components(vec![]), // 移除 Select Menu
                            )
                            .await?;
                    }
                    Err(e) => {
                        interaction
                            .edit_response(
                                &ctx.http,
                                EditInteractionResponse::new()
                                    .content(format!("❌ 切換模型失敗: {}", e))
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
                            .content("❌ 無效的模型格式")
                            .components(vec![]),
                    )
                    .await?;
            }
        }
    }
    Ok(())
}
