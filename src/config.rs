use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct Config {
    pub discord_token: String,
    pub debug_level: Option<String>,
    #[serde(default = "default_lang")]
    pub language: String,
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
