use super::SlashCommand;
use async_trait::async_trait;
use serenity::all::{
    ActionRowComponent, CommandInteraction, Context, CreateActionRow, CreateInputText,
    CreateInteractionResponse, CreateModal, CreateSelectMenu, CreateSelectMenuKind,
    CreateSelectMenuOption, EditInteractionResponse, InputTextStyle, ModalInteraction,
};

use crate::agent::AgentType;

const ASSISTANT_NAME_MAX_CHARS: usize = 48;

pub struct ConfigCommand;

#[async_trait]
impl SlashCommand for ConfigCommand {
    fn name(&self) -> &'static str {
        "config"
    }

    fn description(&self, i18n: &crate::i18n::I18n) -> String {
        i18n.get("cmd_config_desc")
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
        let backend = channel_config.get_agent_type(&channel_id_str);
        let assistant_name = channel_config
            .channels
            .get(&channel_id_str)
            .and_then(|e| e.assistant_name.clone())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| state.config.assistant_name.clone());
        let mention_only = state
            .auth
            .get_channel_mention_only(&channel_id_str)
            .unwrap_or(true);

        let i18n = state.i18n.read().await;
        let status = i18n.get_args(
            "config_current",
            &[
                backend.to_string(),
                if mention_only {
                    i18n.get("config_mention_on")
                } else {
                    i18n.get("config_mention_off")
                },
                assistant_name,
            ],
        );

        let backend_menu = CreateSelectMenu::new(
            "config_backend_select",
            CreateSelectMenuKind::String {
                options: vec![
                    CreateSelectMenuOption::new(i18n.get("agent_choice_kilo"), "kilo"),
                    CreateSelectMenuOption::new(i18n.get("agent_choice_copilot"), "copilot"),
                    CreateSelectMenuOption::new(i18n.get("agent_choice_pi"), "pi"),
                    CreateSelectMenuOption::new(i18n.get("agent_choice_opencode"), "opencode"),
                ],
            },
        )
        .placeholder(i18n.get("config_backend_placeholder"))
        .min_values(1)
        .max_values(1);

        let mention_menu = CreateSelectMenu::new(
            "config_mention_select",
            CreateSelectMenuKind::String {
                options: vec![
                    CreateSelectMenuOption::new(i18n.get("config_mention_on"), "on"),
                    CreateSelectMenuOption::new(i18n.get("config_mention_off"), "off"),
                ],
            },
        )
        .placeholder(i18n.get("config_mention_placeholder"))
        .min_values(1)
        .max_values(1);

        let assistant_menu = CreateSelectMenu::new(
            "config_assistant_select",
            CreateSelectMenuKind::String {
                options: vec![
                    CreateSelectMenuOption::new(i18n.get("config_assistant_default"), "default"),
                    CreateSelectMenuOption::new(i18n.get("config_assistant_custom"), "custom"),
                ],
            },
        )
        .placeholder(i18n.get("config_assistant_placeholder"))
        .min_values(1)
        .max_values(1);

        command
            .edit_response(
                &ctx.http,
                EditInteractionResponse::new()
                    .content(status)
                    .components(vec![
                        CreateActionRow::SelectMenu(backend_menu),
                        CreateActionRow::SelectMenu(mention_menu),
                        CreateActionRow::SelectMenu(assistant_menu),
                    ]),
            )
            .await?;

        Ok(())
    }
}

fn sanitize_assistant_name(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Strip control chars/backticks and neutralize mention marker.
    let mut out = String::new();
    for ch in trimmed.chars() {
        if ch.is_control() || ch == '`' {
            continue;
        }
        if ch == '@' {
            out.push('@');
            out.push('\u{200B}');
            continue;
        }
        out.push(ch);
    }

    let out = out.trim().to_string();
    if out.is_empty() {
        return None;
    }

    let final_name: String = out.chars().take(ASSISTANT_NAME_MAX_CHARS).collect();
    Some(final_name)
}

pub async fn handle_config_select(
    ctx: &Context,
    interaction: &serenity::all::ComponentInteraction,
    state: &crate::AppState,
) -> anyhow::Result<()> {
    let custom_id = interaction.data.custom_id.as_str();
    let value = match &interaction.data.kind {
        serenity::all::ComponentInteractionDataKind::StringSelect { values } => {
            values.first().cloned()
        }
        _ => None,
    };
    let Some(value) = value else {
        return Ok(());
    };

    let channel_id_u64 = interaction.channel_id.get();
    let channel_id_str = interaction.channel_id.to_string();

    if custom_id == "config_assistant_select" && value == "custom" {
        let channel_config = crate::commands::agent::ChannelConfig::load()
            .await
            .unwrap_or_default();
        let current = channel_config
            .channels
            .get(&channel_id_str)
            .and_then(|e| e.assistant_name.clone())
            .unwrap_or_else(|| state.config.assistant_name.clone());

        let i18n = state.i18n.read().await;
        let modal = CreateModal::new(
            "config_assistant_modal",
            i18n.get("config_assistant_modal_title"),
        )
        .components(vec![CreateActionRow::InputText(
            CreateInputText::new(
                InputTextStyle::Short,
                i18n.get("config_assistant_modal_label"),
                "assistant_name",
            )
            .placeholder(i18n.get("config_assistant_modal_hint"))
            .value(current)
            .required(true)
            .max_length(ASSISTANT_NAME_MAX_CHARS as u16),
        )]);

        interaction
            .create_response(&ctx.http, CreateInteractionResponse::Modal(modal))
            .await?;
        return Ok(());
    }

    interaction.defer_ephemeral(&ctx.http).await?;

    match custom_id {
        "config_backend_select" => {
            let selected: AgentType = value.parse()?;
            let mut channel_config = crate::commands::agent::ChannelConfig::load()
                .await
                .unwrap_or_default();
            let current = channel_config.get_agent_type(&channel_id_str);

            let msg = if current == selected {
                let i18n = state.i18n.read().await;
                i18n.get_args("agent_already", &[selected.to_string()])
            } else {
                channel_config.set_agent_type(&channel_id_str, selected.clone());
                state.session_manager.remove_session(channel_id_u64).await;

                match state
                    .session_manager
                    .get_or_create_session(channel_id_u64, selected.clone(), &state.backend_manager)
                    .await
                {
                    Ok(_) => {
                        channel_config.save().await?;
                        let i18n = state.i18n.read().await;
                        i18n.get_args("config_backend_set", &[selected.to_string()])
                    }
                    Err(e) => {
                        let i18n = state.i18n.read().await;
                        crate::commands::agent::build_backend_error_message(
                            &i18n,
                            selected,
                            &e.to_string(),
                            state.config.opencode.port,
                        )
                    }
                }
            };

            interaction
                .edit_response(&ctx.http, EditInteractionResponse::new().content(msg))
                .await?;
        }
        "config_mention_select" => {
            let enable = value == "on";
            let msg = {
                let i18n = state.i18n.read().await;
                match state.auth.set_mention_only(&channel_id_str, enable) {
                    Ok(_) => i18n.get(if enable { "mention_on" } else { "mention_off" }),
                    Err(_) => i18n.get("mention_not_auth"),
                }
            };

            interaction
                .edit_response(&ctx.http, EditInteractionResponse::new().content(msg))
                .await?;
        }
        "config_assistant_select" => {
            let mut channel_config = crate::commands::agent::ChannelConfig::load()
                .await
                .unwrap_or_default();
            channel_config.set_agent_type(
                &channel_id_str,
                channel_config.get_agent_type(&channel_id_str),
            );
            if let Some(entry) = channel_config.channels.get_mut(&channel_id_str) {
                entry.assistant_name = None;
            }
            channel_config.save().await?;

            let msg = {
                let i18n = state.i18n.read().await;
                i18n.get_args("config_assistant_set", &[state.config.assistant_name.clone()])
            };

            interaction
                .edit_response(&ctx.http, EditInteractionResponse::new().content(msg))
                .await?;
        }
        _ => {}
    }

    Ok(())
}

pub async fn handle_assistant_modal_submit(
    ctx: &Context,
    interaction: &ModalInteraction,
    state: &crate::AppState,
) -> anyhow::Result<()> {
    interaction.defer_ephemeral(&ctx.http).await?;

    let mut raw = String::new();
    for row in &interaction.data.components {
        for component in &row.components {
            if let ActionRowComponent::InputText(text) = component {
                if text.custom_id == "assistant_name" {
                    raw = text.value.clone().unwrap_or_default();
                }
            }
        }
    }

    let Some(safe_name) = sanitize_assistant_name(&raw) else {
        let msg = {
            let i18n = state.i18n.read().await;
            i18n.get("config_assistant_invalid")
        };
        interaction
            .edit_response(&ctx.http, EditInteractionResponse::new().content(msg))
            .await?;
        return Ok(());
    };

    let channel_id = interaction.channel_id.to_string();
    let mut channel_config = crate::commands::agent::ChannelConfig::load()
        .await
        .unwrap_or_default();
    channel_config.set_agent_type(&channel_id, channel_config.get_agent_type(&channel_id));
    if let Some(entry) = channel_config.channels.get_mut(&channel_id) {
        entry.assistant_name = Some(safe_name.clone());
    }
    channel_config.save().await?;

    let msg = {
        let i18n = state.i18n.read().await;
        i18n.get_args("config_assistant_set", &[safe_name])
    };

    interaction
        .edit_response(&ctx.http, EditInteractionResponse::new().content(msg))
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::sanitize_assistant_name;

    #[test]
    fn test_sanitize_assistant_name_strips_controls_and_limits_length() {
        let input = "  bad`name\n@everyone\u{0007}  ";
        let got = sanitize_assistant_name(input).unwrap_or_default();
        assert!(!got.contains('`'));
        assert!(!got.contains('\n'));
        assert!(got.contains("@\u{200B}everyone"));
    }

    #[test]
    fn test_sanitize_assistant_name_rejects_empty() {
        assert!(sanitize_assistant_name("   \n\t").is_none());
    }

    #[test]
    fn test_sanitize_assistant_name_accepts_cjk() {
        let input = "測試助手名稱-中文";
        let got = sanitize_assistant_name(input).unwrap_or_default();
        assert_eq!(got, input);
    }
}
