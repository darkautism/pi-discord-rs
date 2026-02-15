use super::SlashCommand;
use async_trait::async_trait;
use serenity::all::{
    ButtonStyle, CommandInteraction, CommandOptionType, ComponentInteraction, Context,
    CreateActionRow, CreateButton, CreateCommandOption, EditInteractionResponse,
};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::info;

use crate::agent::{AgentType, OpencodeAgent, PiAgent};
use crate::config::Config;
use crate::migrate;

pub struct AgentCommand;

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default)]
pub struct ChannelConfig {
    #[serde(default)]
    pub channels: HashMap<String, ChannelEntry>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct ChannelEntry {
    #[serde(default)]
    pub agent_type: AgentType,
    #[serde(default)]
    pub authorized_at: String,
    #[serde(default)]
    pub mention_only: bool,
}

impl ChannelConfig {
    pub async fn load() -> anyhow::Result<Self> {
        let path = super::super::migrate::get_channel_config_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = tokio::fs::read_to_string(&path).await?;
        let config: Self = serde_json::from_str(&content)?;
        Ok(config)
    }

    pub async fn save(&self) -> anyhow::Result<()> {
        let path = super::super::migrate::get_channel_config_path();
        let content = serde_json::to_string_pretty(self)?;
        tokio::fs::write(&path, content).await?;
        Ok(())
    }

    pub fn get_agent_type(&self, channel_id: &str) -> AgentType {
        self.channels
            .get(channel_id)
            .map(|e| e.agent_type.clone())
            .unwrap_or_default()
    }

    pub fn set_agent_type(&mut self, channel_id: &str, agent_type: AgentType) {
        let entry = self.channels.entry(channel_id.to_string()).or_insert_with(|| ChannelEntry {
            agent_type: agent_type.clone(),
            authorized_at: chrono::Utc::now().to_rfc3339(),
            mention_only: true,
        });
        entry.agent_type = agent_type;
    }
}

#[async_trait]
impl SlashCommand for AgentCommand {
    fn name(&self) -> &'static str {
        "agent"
    }

    fn description(&self) -> &'static str {
        "切換 AI Agent backend"
    }

    fn options(&self) -> Vec<CreateCommandOption> {
        vec![CreateCommandOption::new(
            CommandOptionType::String,
            "backend",
            "選擇 agent: pi 或 opencode",
        )
        .required(true)
        .add_string_choice("Pi (本地 RPC)", "pi")
        .add_string_choice("OpenCode (HTTP API)", "opencode")]
    }

    async fn execute(
        &self,
        ctx: &Context,
        command: &CommandInteraction,
        _agent: Arc<dyn crate::agent::AiAgent>,
    ) -> anyhow::Result<()> {
        // 先 defer，避免 3 秒超時
        command.defer_ephemeral(&ctx.http).await?;

        let new_agent_type = command
            .data
            .options
            .iter()
            .find(|o| o.name == "backend")
            .and_then(|o| o.value.as_str())
            .unwrap_or("pi");

        let new_agent_type: AgentType = new_agent_type.parse()?;
        let channel_id = command.channel_id.to_string();

        // 檢查當前 agent 類型
        let config = ChannelConfig::load().await?;
        let current_agent = config.get_agent_type(&channel_id);

        if current_agent == new_agent_type {
            command
                .edit_response(
                    &ctx.http,
                    EditInteractionResponse::new()
                        .content(format!("ℹ️ 已經在使用 {} backend", new_agent_type)),
                )
                .await?;
            return Ok(());
        }

        // 發送確認訊息 + 按鈕
        command
            .edit_response(
                &ctx.http,
                EditInteractionResponse::new()
                    .content(format!(
                        "⚠️ 切換至 {} backend 將會清除當前對話歷史，確定要繼續嗎？",
                        new_agent_type
                    ))
                    .components(vec![CreateActionRow::Buttons(vec![
                        CreateButton::new(format!("agent_confirm:{}", new_agent_type))
                            .label("✅ 確定清除")
                            .style(ButtonStyle::Danger),
                        CreateButton::new("agent_cancel")
                            .label("❌ 取消")
                            .style(ButtonStyle::Secondary),
                    ])]),
            )
            .await?;

        Ok(())
    }
}

pub async fn handle_button(
    ctx: &Context,
    interaction: &ComponentInteraction,
    state: &crate::AppState,
) -> anyhow::Result<()> {
    // 先 defer，避免 3 秒超時
    interaction.defer_ephemeral(&ctx.http).await?;

    let custom_id = interaction.data.custom_id.as_str();

    if custom_id == "agent_cancel" {
        interaction
            .edit_response(
                &ctx.http,
                EditInteractionResponse::new()
                    .content("❌ 已取消切換")
                    .components(vec![]),
            )
            .await?;
        return Ok(());
    }

    if let Some(agent_type_str) = custom_id.strip_prefix("agent_confirm:") {
        let agent_type: AgentType = agent_type_str.parse()?;
        let channel_id = interaction.channel_id.to_string();
        let channel_id_u64 = interaction.channel_id.get();

        // 先更新配置
        let mut channel_config = ChannelConfig::load().await?;
        channel_config.set_agent_type(&channel_id, agent_type.clone());
        
        // 移除舊 session
        state.session_manager.remove_session(channel_id_u64).await;
        
        // 測試並創建新 session
        match state.session_manager.get_or_create_session(channel_id_u64, agent_type.clone()).await {
            Ok(_) => {
                // 連接成功，保存配置
                channel_config.save().await?;
                info!(
                    "Channel {} switched to {} backend",
                    channel_id, agent_type
                );

                interaction
                    .edit_response(
                        &ctx.http,
                        EditInteractionResponse::new()
                            .content(format!(
                                "✅ 已切換至 {} backend\n新對話已開始",
                                agent_type
                            ))
                            .components(vec![]),
                    )
                    .await?;
            }
            Err(e) => {
                // 連接失敗，不保存配置（回滾）
                let error_msg = if agent_type == AgentType::Opencode {
                    format!(
                        "❌ 無法連線至 OpenCode: {}\n\n請確認已在目標機器執行:\n```\nopencode serve --port {}\n```",
                        e, state.config.opencode.port
                    )
                } else {
                    format!("❌ 無法連線至 Pi: {}", e)
                };

                interaction
                    .edit_response(
                        &ctx.http,
                        EditInteractionResponse::new()
                            .content(error_msg)
                            .components(vec![]),
                    )
                    .await?;
            }
        }
    }

    Ok(())
}
