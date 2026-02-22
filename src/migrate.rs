use serde_json::json;
use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::info;

const CURRENT_VERSION: u32 = 1;
const OLD_BASE_DIR: &str = ".pi/discord-rs";
const NEW_BASE_DIR: &str = ".agent-discord-rs";
pub const BASE_DIR_ENV: &str = "AGENT_DISCORD_BASE_DIR";

pub async fn run_migrations() -> anyhow::Result<()> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("No home directory"))?;
    let old_dir = home.join(OLD_BASE_DIR);
    let new_dir = home.join(NEW_BASE_DIR);
    let version_file = new_dir.join(".version");

    // 檢查是否已經遷移過
    let current_version = read_version(&version_file).await;
    if current_version >= CURRENT_VERSION {
        return Ok(());
    }

    // 檢查是否需要遷移
    let needs_migration = if old_dir.exists() && !new_dir.exists() {
        // 舊資料存在且新目錄不存在 - 完整遷移
        true
    } else if old_dir.exists() && new_dir.exists() {
        // 新目錄已存在，檢查 config 是否需要遷移 token
        let new_config = new_dir.join("config.toml");
        let old_config = old_dir.join("config.toml");

        if old_config.exists() && new_config.exists() {
            // 檢查新 config 是否為預設值
            let new_content = fs::read_to_string(&new_config).await.unwrap_or_default();
            if new_content.contains("YOUR_DISCORD_TOKEN_HERE") {
                // 檢查舊 config 是否有有效 token
                let old_content = fs::read_to_string(&old_config).await.unwrap_or_default();
                if !old_content.contains("YOUR_DISCORD_TOKEN_HERE") {
                    info!(
                        "🔄 Detected placeholder token in new config, migrating from old config..."
                    );
                    true
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        }
    } else {
        false
    };

    if needs_migration {
        if !new_dir.exists() {
            info!("🔄 Detected old version data, starting migration...");
            migrate_v0_to_v1(&old_dir, &new_dir).await?;
            info!("✅ Data migration completed");
        } else {
            info!("🔄 Updating config from old version...");
            migrate_config_only(&old_dir, &new_dir).await?;
            info!("✅ Config updated");
        }
    }

    // 始終檢查是否需要遷移認證資料（即使 config 不需要遷移）
    if old_dir.exists() && new_dir.exists() {
        migrate_auth_and_sessions(&old_dir, &new_dir).await?;
    }

    if !new_dir.exists() {
        // 全新安裝
        fs::create_dir_all(&new_dir).await?;
        fs::create_dir_all(new_dir.join("sessions").join("pi")).await?;
        fs::create_dir_all(new_dir.join("sessions").join("opencode")).await?;
        fs::create_dir_all(new_dir.join("sessions").join("copilot")).await?;
        fs::create_dir_all(new_dir.join("prompts")).await?;
        fs::create_dir_all(new_dir.join("uploads")).await?;
    }

    write_version(&version_file, CURRENT_VERSION).await?;
    Ok(())
}

async fn read_version(path: &PathBuf) -> u32 {
    match fs::read_to_string(path).await {
        Ok(content) => content.trim().parse().unwrap_or(0),
        Err(_) => 0,
    }
}

async fn write_version(path: &PathBuf, version: u32) -> anyhow::Result<()> {
    fs::write(path, version.to_string()).await?;
    Ok(())
}

async fn migrate_config_only(old_dir: &Path, new_dir: &Path) -> anyhow::Result<()> {
    // 只遷移 config.toml 中的 token
    let old_config = old_dir.join("config.toml");
    let new_config = new_dir.join("config.toml");

    if old_config.exists() {
        let old_content = fs::read_to_string(&old_config).await?;
        let mut new_content = fs::read_to_string(&new_config).await?;

        // 提取舊 config 的 token
        if let Some(token_line) = old_content.lines().find(|l| l.starts_with("discord_token")) {
            if let Some(token) = token_line.split('=').nth(1) {
                let token = token.trim().trim_matches('"');
                // 替換新 config 的 token
                new_content = new_content.replace(
                    r#"discord_token = "YOUR_DISCORD_TOKEN_HERE""#,
                    &format!(r#"discord_token = "{}""#, token),
                );
                fs::write(&new_config, new_content).await?;
            }
        }
    }

    Ok(())
}

async fn migrate_auth_and_sessions(old_dir: &Path, new_dir: &Path) -> anyhow::Result<()> {
    // 遷移認證資料
    let old_registry = old_dir.join("registry.json");
    let new_auth = new_dir.join("auth.json");

    if !old_registry.exists() {
        return Ok(());
    }

    // 讀取舊資料
    let content = fs::read_to_string(&old_registry).await?;
    let old_data: serde_json::Value = serde_json::from_str(&content)?;

    // 檢查新資料是否需要更新（如果 users 或 channels 為空，則需要遷移）
    let need_migration = if new_auth.exists() {
        let new_content = fs::read_to_string(&new_auth).await.unwrap_or_default();
        let new_data: serde_json::Value = serde_json::from_str(&new_content).unwrap_or(json!({}));

        let old_users = old_data
            .get("users")
            .and_then(|v| v.as_object())
            .map(|m| m.len())
            .unwrap_or(0);
        let old_channels = old_data
            .get("channels")
            .and_then(|v| v.as_object())
            .map(|m| m.len())
            .unwrap_or(0);
        let new_users = new_data
            .get("users")
            .and_then(|v| v.as_object())
            .map(|m| m.len())
            .unwrap_or(0);
        let new_channels = new_data
            .get("channels")
            .and_then(|v| v.as_object())
            .map(|m| m.len())
            .unwrap_or(0);

        // 如果舊資料比新資料多，需要重新遷移
        old_users > new_users || old_channels > new_channels
    } else {
        true
    };

    if need_migration {
        info!("🔐 Migrating authentication data...");

        let mut new_channels = serde_json::Map::new();
        if let Some(channels) = old_data.get("channels").and_then(|v| v.as_object()) {
            for (channel_id, entry) in channels {
                let mut new_entry = entry.clone();
                new_entry["agent_type"] = json!("pi");
                new_channels.insert(channel_id.clone(), new_entry);
            }
        }

        let new_data = json!({
            "users": old_data.get("users").unwrap_or(&json!({})),
            "channels": new_channels,
        });

        fs::write(&new_auth, serde_json::to_string_pretty(&new_data)?).await?;
        info!("✅ Authentication data migrated successfully");
    }

    // 遷移 Pi sessions
    let old_sessions = old_dir.join("sessions");
    let new_pi_sessions = new_dir.join("sessions").join("pi");
    if old_sessions.exists() {
        fs::create_dir_all(&new_pi_sessions).await?;
        let mut entries = fs::read_dir(&old_sessions).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.is_file() {
                let filename = entry.file_name();
                let dest = new_pi_sessions.join(&filename);
                if !dest.exists() {
                    fs::copy(&path, dest).await?;
                }
            }
        }
    }

    // 遷移 prompts
    let old_prompts = old_dir.join("prompts");
    let new_prompts = new_dir.join("prompts");
    if old_prompts.exists() {
        fs::create_dir_all(&new_prompts).await?;
        let mut entries = fs::read_dir(&old_prompts).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.is_file() {
                let filename = entry.file_name();
                let dest = new_prompts.join(&filename);
                if !dest.exists() {
                    fs::copy(&path, dest).await?;
                }
            }
        }
    }

    Ok(())
}

async fn migrate_v0_to_v1(old_dir: &Path, new_dir: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(&new_dir).await?;
    fs::create_dir_all(new_dir.join("sessions").join("pi")).await?;
    fs::create_dir_all(new_dir.join("sessions").join("opencode")).await?;
    fs::create_dir_all(new_dir.join("sessions").join("copilot")).await?;
    fs::create_dir_all(new_dir.join("prompts")).await?;
    fs::create_dir_all(new_dir.join("uploads")).await?;

    // 遷移 config.toml
    let old_config = old_dir.join("config.toml");
    let new_config = new_dir.join("config.toml");
    if old_config.exists() {
        info!("📄 Migrating config.toml...");
        let content = fs::read_to_string(&old_config).await?;

        // 添加 opencode 配置區塊（如果不存在）
        let final_content = if !content.contains("[opencode]") {
            let opencode_config = r#"

[opencode]
host = "127.0.0.1"
port = 4096
# password = "your-password"  # Uncomment if using OPENCODE_SERVER_PASSWORD
"#;
            format!("{}{}", content, opencode_config)
        } else {
            content
        };

        fs::write(&new_config, final_content).await?;
    } else {
        // 創建默認配置
let default_config = r#"discord_token = "YOUR_DISCORD_TOKEN_HERE"
debug_level = "INFO"
language = "zh-TW"
assistant_name = "Agent"

[opencode]
host = "127.0.0.1"
port = 4096
# password = "your-password"
"#;
        fs::write(&new_config, default_config).await?;
    }

    // 遷移認證資料、session 和 prompts
    migrate_auth_and_sessions(old_dir, new_dir).await?;

    // 創建 channel_config.json
    let channel_config = json!({
        "version": 1,
        "channels": {}
    });
    fs::write(
        new_dir.join("channel_config.json"),
        serde_json::to_string_pretty(&channel_config)?,
    )
    .await?;

    info!("✅ Migration from v0 to v1 completed");
    Ok(())
}

pub fn get_base_dir() -> PathBuf {
    if let Ok(v) = std::env::var(BASE_DIR_ENV) {
        if !v.trim().is_empty() {
            return PathBuf::from(v);
        }
    }

    #[cfg(test)]
    {
        // 測試模式下禁止使用真實目錄，強制讓未隔離的測試崩潰
        panic!(
            "FATAL: Test tried to access real data directory! Use a temporary directory instead."
        );
    }
    #[cfg(not(test))]
    {
        dirs::home_dir()
            .expect("No home directory")
            .join(NEW_BASE_DIR)
    }
}

pub fn get_config_path() -> PathBuf {
    get_base_dir().join("config.toml")
}

pub fn get_channel_config_path() -> PathBuf {
    get_base_dir().join("channel_config.json")
}

pub fn get_sessions_dir(agent_type: &str) -> PathBuf {
    get_base_dir().join("sessions").join(agent_type)
}

pub fn get_prompts_dir() -> PathBuf {
    get_base_dir().join("prompts")
}

pub fn get_uploads_dir() -> PathBuf {
    get_base_dir().join("uploads")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};
    use tempfile::tempdir;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn test_get_base_dir_uses_env_override() {
        let _guard = env_lock().lock().expect("lock");
        let dir = tempdir().expect("tempdir");
        // SAFETY: tests serialize env writes via global mutex
        unsafe { std::env::set_var(BASE_DIR_ENV, dir.path()) };
        let got = get_base_dir();
        assert_eq!(got, dir.path());
        // SAFETY: tests serialize env writes via global mutex
        unsafe { std::env::remove_var(BASE_DIR_ENV) };
    }

    #[tokio::test]
    async fn test_migrate_config_only_replaces_placeholder_token() {
        let old = tempdir().expect("old");
        let newd = tempdir().expect("new");
        let old_cfg = old.path().join("config.toml");
        let new_cfg = newd.path().join("config.toml");

        fs::write(&old_cfg, "discord_token = \"REAL_TOKEN\"").await.expect("write old");
        fs::write(
            &new_cfg,
            "discord_token = \"YOUR_DISCORD_TOKEN_HERE\"\nlanguage = \"zh-TW\"",
        )
        .await
        .expect("write new");

        migrate_config_only(old.path(), newd.path())
            .await
            .expect("migrate config");
        let updated = fs::read_to_string(new_cfg).await.expect("read updated");
        assert!(updated.contains("REAL_TOKEN"));
        assert!(!updated.contains("YOUR_DISCORD_TOKEN_HERE"));
    }

    #[tokio::test]
    async fn test_migrate_auth_and_sessions_transfers_registry_sessions_and_prompts() {
        let old = tempdir().expect("old");
        let newd = tempdir().expect("new");

        fs::create_dir_all(old.path().join("sessions"))
            .await
            .expect("mkdir sessions");
        fs::create_dir_all(old.path().join("prompts"))
            .await
            .expect("mkdir prompts");
        fs::write(old.path().join("sessions").join("s1.jsonl"), "abc")
            .await
            .expect("write session");
        fs::write(old.path().join("prompts").join("p1.txt"), "prompt")
            .await
            .expect("write prompt");
        fs::write(
            old.path().join("registry.json"),
            r#"{"users":{"u1":{"authorized_at":"2026-01-01T00:00:00Z","mention_only":false}},"channels":{"c1":{"authorized_at":"2026-01-01T00:00:00Z","mention_only":true}}}"#,
        )
        .await
        .expect("write registry");

        migrate_auth_and_sessions(old.path(), newd.path())
            .await
            .expect("migrate auth");

        let auth = fs::read_to_string(newd.path().join("auth.json"))
            .await
            .expect("read auth");
        assert!(auth.contains("\"users\""));
        assert!(auth.contains("\"channels\""));
        assert!(auth.contains("\"agent_type\": \"pi\""));
        assert!(newd
            .path()
            .join("sessions")
            .join("pi")
            .join("s1.jsonl")
            .exists());
        assert!(newd.path().join("prompts").join("p1.txt").exists());
    }

    #[tokio::test]
    async fn test_migrate_v0_to_v1_creates_expected_layout() {
        let old = tempdir().expect("old");
        let newd = tempdir().expect("new");
        fs::create_dir_all(old.path()).await.expect("mkdir old");

        migrate_v0_to_v1(old.path(), newd.path())
            .await
            .expect("migrate");

        assert!(newd.path().join("sessions").join("pi").exists());
        assert!(newd.path().join("sessions").join("opencode").exists());
        assert!(newd.path().join("sessions").join("copilot").exists());
        assert!(newd.path().join("prompts").exists());
        assert!(newd.path().join("uploads").exists());

        let cfg = fs::read_to_string(newd.path().join("config.toml"))
            .await
            .expect("read cfg");
        assert!(cfg.contains("assistant_name = \"Agent\""));
    }
}
