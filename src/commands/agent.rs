use super::SlashCommand;
use async_trait::async_trait;
use serenity::all::{
    ButtonStyle, CommandInteraction, CommandOptionType, ComponentInteraction, Context,
    CreateActionRow, CreateButton, CreateCommandOption, EditInteractionResponse,
};
use std::collections::HashMap;
use tracing::info;

use crate::agent::AgentType;

pub struct AgentCommand;

fn is_binary_not_found(error_text: &str) -> bool {
    let lower = error_text.to_lowercase();
    lower.contains("no such file or directory")
        || lower.contains("not found")
        || lower.contains("enoent")
}

pub fn build_backend_error_message(
    i18n: &crate::i18n::I18n,
    agent_type: AgentType,
    error_text: &str,
    port: u16,
) -> String {
    let backend = agent_type.to_string();
    let base = i18n.get_args(
        "backend_start_failed",
        &[backend.clone(), error_text.to_string()],
    );

    if is_binary_not_found(error_text) {
        let install_cmd = match agent_type {
            AgentType::Pi => "npm install -g @mariozechner/pi-coding-agent",
            AgentType::Opencode => "npm install -g @opencode-ai/cli",
            AgentType::Kilo => "npm install -g @kilocode/cli",
            AgentType::Copilot => "npm install -g @github/copilot",
        };
        return format!(
            "{}\n\n{}:\n```bash\n{}\n```",
            base,
            i18n.get("backend_install_hint"),
            install_cmd
        );
    }

    match agent_type {
        AgentType::Opencode => format!(
            "{}\n\n{}:\n```bash\nopencode serve --port {}\n```",
            base,
            i18n.get("backend_start_hint"),
            port
        ),
        AgentType::Kilo => format!(
            "{}\n\n{}:\n```bash\nkilo serve --port {}\n```",
            base,
            i18n.get("backend_start_hint"),
            port
        ),
        AgentType::Copilot => {
            let lower = error_text.to_lowercase();
            let auth_hint = if lower.contains("auth")
                || lower.contains("login")
                || lower.contains("unauthorized")
                || lower.contains("not authenticated")
            {
                i18n.get("copilot_login_hint")
            } else {
                i18n.get("copilot_runtime_hint")
            };
            format!(
                "{}\n\n{}\n{}",
                base,
                i18n.get("copilot_managed_hint"),
                auth_hint
            )
        }
        AgentType::Pi => format!("{}\n\n{}", base, i18n.get("pi_runtime_hint")),
    }
}

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
    // 通用 Session ID，不再區分 kilo 或 opencode
    #[serde(default, alias = "kilo_session_id")]
    pub session_id: Option<String>,
    pub model_provider: Option<String>,
    pub model_id: Option<String>,
    pub assistant_name: Option<String>,
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
        let entry = self
            .channels
            .entry(channel_id.to_string())
            .or_insert_with(|| ChannelEntry {
                agent_type: agent_type.clone(),
                authorized_at: chrono::Utc::now().to_rfc3339(),
                mention_only: true,
                session_id: None,
                model_provider: None,
                model_id: None,
                assistant_name: None,
            });
        entry.agent_type = agent_type;
    }
}

#[async_trait]
impl SlashCommand for AgentCommand {
    fn name(&self) -> &'static str {
        "agent"
    }

    fn description(&self, i18n: &crate::i18n::I18n) -> String {
        i18n.get("cmd_agent_desc")
    }

    fn options(&self, i18n: &crate::i18n::I18n) -> Vec<CreateCommandOption> {
        vec![CreateCommandOption::new(
            CommandOptionType::String,
            "backend",
            i18n.get("cmd_agent_opt_backend"),
        )
        .required(true)
        .add_string_choice(i18n.get("agent_choice_kilo"), "kilo")
        .add_string_choice(i18n.get("agent_choice_copilot"), "copilot")
        .add_string_choice(i18n.get("agent_choice_pi"), "pi")
        .add_string_choice(i18n.get("agent_choice_opencode"), "opencode")]
    }

    async fn execute(
        &self,
        ctx: &Context,
        command: &CommandInteraction,
        state: &crate::AppState,
    ) -> anyhow::Result<()> {
        // 先 defer，避免 3 秒超時
        command.defer_ephemeral(&ctx.http).await?;

        let new_agent_type_str = command
            .data
            .options
            .iter()
            .find(|o| o.name == "backend")
            .and_then(|o| o.value.as_str())
            .unwrap_or("pi");

        let new_agent_type: AgentType = new_agent_type_str.parse()?;
        let channel_id = command.channel_id.to_string();

        // 檢查當前 agent 類型
        let config = ChannelConfig::load().await?;
        let current_agent = config.get_agent_type(&channel_id);

        let i18n = state.i18n.read().await;

        if current_agent == new_agent_type {
            let msg = i18n.get_args("agent_already", &[new_agent_type.to_string()]);
            command
                .edit_response(&ctx.http, EditInteractionResponse::new().content(msg))
                .await?;
            return Ok(());
        }

        // 發送確認訊息 + 按鈕
        let confirm_msg = i18n.get_args("agent_confirm", &[new_agent_type.to_string()]);
        command
            .edit_response(
                &ctx.http,
                EditInteractionResponse::new()
                    .content(confirm_msg)
                    .components(vec![CreateActionRow::Buttons(vec![
                        CreateButton::new(format!("agent_confirm:{}", new_agent_type))
                            .label(i18n.get("agent_confirm_btn"))
                            .style(ButtonStyle::Danger),
                        CreateButton::new("agent_cancel")
                            .label(i18n.get("agent_cancel_btn"))
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
    let i18n = state.i18n.read().await;

    if custom_id == "agent_cancel" {
        interaction
            .edit_response(
                &ctx.http,
                EditInteractionResponse::new()
                    .content(i18n.get("agent_cancelled"))
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
        match state
            .session_manager
            .get_or_create_session(channel_id_u64, agent_type.clone(), &state.backend_manager)
            .await
        {
            Ok(_) => {
                // 連接成功，保存配置
                channel_config.save().await?;
                info!("Channel {} switched to {} backend", channel_id, agent_type);

                interaction
                    .edit_response(
                        &ctx.http,
                        EditInteractionResponse::new()
                            .content(i18n.get_args("agent_switched", &[agent_type.to_string()]))
                            .components(vec![]),
                    )
                    .await?;
            }
            Err(e) => {
                // 連接失敗，不保存配置（回滾）
                let error_text = e.to_string();
                let error_msg = build_backend_error_message(
                    &i18n,
                    agent_type,
                    &error_text,
                    state.config.opencode.port,
                );

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

#[cfg(test)]
mod tests {
    use super::{build_backend_error_message, is_binary_not_found, ChannelEntry};
    use crate::agent::AgentType;
    use crate::i18n::I18n;

    #[test]
    fn test_binary_not_found_detection() {
        assert!(is_binary_not_found("Spawn failed: No such file or directory"));
        assert!(is_binary_not_found("ENOENT: not found"));
        assert!(!is_binary_not_found("connection refused"));
    }

    #[test]
    fn test_backend_error_message_for_missing_binary_has_install_hint() {
        let i18n = I18n::new("en");
        let msg = build_backend_error_message(
            &i18n,
            AgentType::Opencode,
            "Spawn failed: No such file or directory",
            4096,
        );
        assert!(msg.contains("Install the backend first"));
        assert!(msg.contains("npm install -g @opencode-ai/cli"));
    }

    #[test]
    fn test_backend_error_message_for_opencode_has_start_command() {
        let i18n = I18n::new("en");
        let msg = build_backend_error_message(&i18n, AgentType::Opencode, "connection refused", 4096);
        assert!(msg.contains("opencode serve --port 4096"));
        assert!(msg.contains("Failed to start opencode backend"));
    }

    #[test]
    fn test_channel_entry_supports_legacy_kilo_session_id_alias() {
        let legacy = r#"{
            "agent_type":"kilo",
            "authorized_at":"2025-01-01T00:00:00Z",
            "mention_only":true,
            "kilo_session_id":"sid-legacy",
            "model_provider":null,
            "model_id":null
        }"#;
        let entry: ChannelEntry = serde_json::from_str(legacy).expect("legacy json should parse");
        assert_eq!(entry.session_id.as_deref(), Some("sid-legacy"));

        let serialized = serde_json::to_string(&entry).expect("serialize");
        assert!(serialized.contains("\"session_id\""));
        assert!(!serialized.contains("\"kilo_session_id\""));
    }
}
