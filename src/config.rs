use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Clone, Debug, Default)]
pub struct Config {
    pub discord_token: String,
    pub debug_level: Option<String>,
    #[serde(default = "default_lang")]
    pub language: String,
    #[serde(default = "default_assistant_name")]
    pub assistant_name: String,
    #[serde(default)]
    pub opencode: OpencodeConfig,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct OpencodeConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub password: Option<String>,
}

impl Default for OpencodeConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 4096,
            password: None,
        }
    }
}

fn default_lang() -> String {
    "zh-TW".to_string()
}

fn default_assistant_name() -> String {
    "Agent".to_string()
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    4096
}

impl Config {
    pub async fn load() -> anyhow::Result<Self> {
        let config_path = super::migrate::get_config_path();

        if !config_path.exists() {
            // 創建默認配置
            let default_config = r#"discord_token = "YOUR_DISCORD_TOKEN_HERE"
debug_level = "INFO"
language = "zh-TW"
assistant_name = "Agent"

[opencode]
host = "127.0.0.1"
port = 4096
# password = "your-password"  # Uncomment if using OPENCODE_SERVER_PASSWORD
"#;
            tokio::fs::write(&config_path, default_config).await?;
            anyhow::bail!(
                "Configuration file not found. Created default at: {}. Please edit it.",
                config_path.display()
            );
        }

        let content = tokio::fs::read_to_string(&config_path).await?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::Config;
    use crate::migrate::BASE_DIR_ENV;
    use std::sync::{Mutex, OnceLock};
    use tempfile::tempdir;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[tokio::test]
    async fn test_load_creates_default_config_when_missing() {
        let _guard = env_lock().lock().expect("lock");
        let dir = tempdir().expect("tempdir");
        // SAFETY: serialized by env lock
        unsafe { std::env::set_var(BASE_DIR_ENV, dir.path()) };
        let err = Config::load().await.expect_err("first load should create default and fail");
        assert!(err.to_string().contains("Configuration file not found"));
        assert!(dir.path().join("config.toml").exists());
        // SAFETY: serialized by env lock
        unsafe { std::env::remove_var(BASE_DIR_ENV) };
    }

    #[tokio::test]
    async fn test_load_reads_existing_config() {
        let _guard = env_lock().lock().expect("lock");
        let dir = tempdir().expect("tempdir");
        // SAFETY: serialized by env lock
        unsafe { std::env::set_var(BASE_DIR_ENV, dir.path()) };
        tokio::fs::write(
            dir.path().join("config.toml"),
            r#"discord_token = "abc"
debug_level = "INFO"
language = "en"
assistant_name = "AgentX"

[opencode]
host = "127.0.0.1"
port = 4096
"#,
        )
        .await
        .expect("write config");
        let cfg = Config::load().await.expect("load");
        assert_eq!(cfg.discord_token, "abc");
        assert_eq!(cfg.language, "en");
        assert_eq!(cfg.assistant_name, "AgentX");
        // SAFETY: serialized by env lock
        unsafe { std::env::remove_var(BASE_DIR_ENV) };
    }
}
