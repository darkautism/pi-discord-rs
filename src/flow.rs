use crate::commands::agent::ChannelConfig;
use crate::i18n::I18n;
use crate::ExecStatus;
use serenity::all::MessageType;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModalRoute {
    CronSetup,
    ConfigAssistant,
    Ignore,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComponentRoute {
    Config,
    Agent,
    CronDelete,
    ModelSelect,
    Ignore,
}

pub fn resolve_channel_assistant_name(
    channel_cfg: &ChannelConfig,
    channel_id: &str,
    default_name: &str,
) -> String {
    channel_cfg
        .channels
        .get(channel_id)
        .and_then(|e| e.assistant_name.clone())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| default_name.to_string())
}

pub fn is_supported_message_kind(kind: MessageType) -> bool {
    kind == MessageType::Regular || kind == MessageType::InlineReply
}

pub fn should_process_message(
    is_bot: bool,
    kind: MessageType,
    mention_only: bool,
    is_mentioned: bool,
) -> bool {
    if is_bot || !is_supported_message_kind(kind) {
        return false;
    }
    if mention_only && !is_mentioned {
        return false;
    }
    true
}

pub fn route_modal(custom_id: &str) -> ModalRoute {
    match custom_id {
        "cron_setup" => ModalRoute::CronSetup,
        "config_assistant_modal" => ModalRoute::ConfigAssistant,
        _ => ModalRoute::Ignore,
    }
}

pub fn route_component(custom_id: &str) -> ComponentRoute {
    if custom_id.starts_with("config_") {
        ComponentRoute::Config
    } else if custom_id.starts_with("agent_") {
        ComponentRoute::Agent
    } else if custom_id == "cron_delete_select" {
        ComponentRoute::CronDelete
    } else if custom_id.starts_with("model_select") {
        ComponentRoute::ModelSelect
    } else {
        ComponentRoute::Ignore
    }
}

pub fn build_render_view(
    i18n: &I18n,
    status: &ExecStatus,
    desc: &str,
    assistant_name: &str,
) -> (String, u32, String) {
    match status {
        ExecStatus::Error(e) => (
            i18n.get("api_error"),
            0xff0000,
            format!("{}\n\n{} {}", desc, i18n.get("runtime_error_prefix"), e),
        ),
        ExecStatus::Success => (
            i18n.get_args("agent_response", &[assistant_name.to_string()]),
            0x00ff00,
            if desc.is_empty() {
                i18n.get("done")
            } else {
                desc.to_string()
            },
        ),
        ExecStatus::Running => (
            i18n.get_args("agent_working", &[assistant_name.to_string()]),
            0xFFA500,
            if desc.is_empty() {
                i18n.get("wait")
            } else {
                desc.to_string()
            },
        ),
    }
}

pub fn get_systemd_service_path() -> anyhow::Result<PathBuf> {
    Ok(dirs::config_dir()
        .or_else(dirs::home_dir)
        .ok_or_else(|| anyhow::anyhow!("Cannot determine config/home directory"))?
        .join("systemd")
        .join("user")
        .join("agent-discord-rs.service"))
}

pub fn detect_timezone() -> String {
    std::fs::read_to_string("/etc/timezone")
        .unwrap_or_else(|_| "UTC".to_string())
        .trim()
        .to_string()
}

pub fn build_systemd_service_content(exe_path: &str, augmented_path: &str, tz: &str) -> String {
    format!(
        r#"[Unit]
Description=Agent Discord RS
After=network.target

[Service]
Type=simple
ExecStart={} run
Environment="PATH={}"
Environment="TZ={}"
Restart=on-failure
RestartSec=5s

[Install]
WantedBy=default.target
"#,
        exe_path, augmented_path, tz
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::agent::{ChannelConfig, ChannelEntry};
    use crate::i18n::I18n;
    use chrono::Utc;
    use std::collections::HashMap;

    #[test]
    fn test_resolve_channel_assistant_name_prefers_channel_value() {
        let mut cfg = ChannelConfig {
            channels: HashMap::new(),
        };
        cfg.channels.insert(
            "1".to_string(),
            ChannelEntry {
                agent_type: crate::agent::AgentType::Kilo,
                authorized_at: Utc::now().to_rfc3339(),
                mention_only: true,
                session_id: None,
                model_provider: None,
                model_id: None,
                assistant_name: Some("MyAgent".to_string()),
            },
        );

        let got = resolve_channel_assistant_name(&cfg, "1", "Agent");
        assert_eq!(got, "MyAgent");
        let fallback = resolve_channel_assistant_name(&cfg, "2", "Agent");
        assert_eq!(fallback, "Agent");
    }

    #[test]
    fn test_should_process_message_rules() {
        assert!(!should_process_message(true, MessageType::Regular, false, false));
        assert!(!should_process_message(
            false,
            MessageType::ThreadStarterMessage,
            false,
            false
        ));
        assert!(!should_process_message(false, MessageType::Regular, true, false));
        assert!(should_process_message(
            false,
            MessageType::InlineReply,
            true,
            true
        ));
    }

    #[test]
    fn test_modal_and_component_routing() {
        assert_eq!(route_modal("cron_setup"), ModalRoute::CronSetup);
        assert_eq!(
            route_modal("config_assistant_modal"),
            ModalRoute::ConfigAssistant
        );
        assert_eq!(route_modal("other"), ModalRoute::Ignore);

        assert_eq!(route_component("config_backend_select"), ComponentRoute::Config);
        assert_eq!(route_component("agent_confirm:kilo"), ComponentRoute::Agent);
        assert_eq!(route_component("cron_delete_select"), ComponentRoute::CronDelete);
        assert_eq!(route_component("model_select_0"), ComponentRoute::ModelSelect);
        assert_eq!(route_component("x"), ComponentRoute::Ignore);
    }

    #[test]
    fn test_build_render_view_uses_i18n_values() {
        let i18n = I18n::new("en");
        let (title, color, desc) =
            build_render_view(&i18n, &ExecStatus::Running, "", "AgentX");
        assert!(title.contains("AgentX"));
        assert_eq!(color, 0xFFA500);
        assert_eq!(desc, i18n.get("wait"));

        let (_, err_color, err_desc) =
            build_render_view(&i18n, &ExecStatus::Error("boom".to_string()), "x", "AgentX");
        assert_eq!(err_color, 0xff0000);
        assert!(err_desc.contains("boom"));
    }

    #[test]
    fn test_build_systemd_service_content_contains_fields() {
        let s = build_systemd_service_content("/bin/a", "/usr/bin", "UTC");
        assert!(s.contains("ExecStart=/bin/a run"));
        assert!(s.contains("Environment=\"PATH=/usr/bin\""));
        assert!(s.contains("Environment=\"TZ=UTC\""));
    }
}
