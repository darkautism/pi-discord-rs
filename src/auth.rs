use crate::migrate;
use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use fs2::FileExt;
use rand::distributions::Alphanumeric;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AuthEntry {
    pub authorized_at: DateTime<Utc>,
    #[serde(default)]
    pub mention_only: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Registry {
    #[serde(default)]
    pub users: HashMap<String, AuthEntry>, // user_id -> entry
    #[serde(default)]
    pub channels: HashMap<String, AuthEntry>, // channel_id -> entry
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PendingToken {
    pub token: String,
    pub type_: String, // "user" or "channel"
    pub id: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct PendingStore {
    pub tokens: HashMap<String, PendingToken>, // token -> data
}

pub struct AuthManager {
    auth_path: PathBuf,
    pending_path: PathBuf,
}

impl AuthManager {
    pub fn new() -> Self {
        let base_dir = migrate::get_base_dir();
        fs::create_dir_all(&base_dir).unwrap();
        Self {
            auth_path: base_dir.join("auth.json"),
            pending_path: base_dir.join("pending_tokens.json"),
        }
    }

    fn with_lock<T, F>(&self, path: PathBuf, default: T, f: F) -> Result<T>
    where
        T: serde::de::DeserializeOwned + serde::Serialize + Default,
        F: FnOnce(&mut T) -> Result<()>,
    {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&path)?;

        file.lock_exclusive()?;

        // Read
        let mut content = String::new();
        let mut reader = std::io::BufReader::new(&file);
        reader.read_to_string(&mut content)?;

        let mut data: T = if content.trim().is_empty() {
            default
        } else {
            serde_json::from_str(&content).unwrap_or_else(|_| default)
        };

        // Modify
        f(&mut data)?;

        // Write
        let json = serde_json::to_string_pretty(&data)?;
        let mut file = file; // Rebind as mutable for writing
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(json.as_bytes())?;

        file.unlock()?;
        Ok(data)
    }

    pub fn is_authorized(&self, user_id: &str, channel_id: &str) -> (bool, bool) {
        // (authorized, mention_only)
        if let Ok(content) = fs::read_to_string(&self.auth_path) {
            if let Ok(reg) = serde_json::from_str::<Registry>(&content) {
                // Check User
                if reg.users.get(user_id).is_some() {
                    return (true, false); // User auth overrides channel mention_only setting
                }
                // Check Channel
                if let Some(entry) = reg.channels.get(channel_id) {
                    return (true, entry.mention_only);
                }
            }
        }
        (false, false)
    }

    pub async fn is_authorized_with_thread(
        &self,
        ctx: &serenity::all::Context,
        user_id: &str,
        channel_id: serenity::model::id::ChannelId,
    ) -> (bool, bool) {
        let id_str = channel_id.to_string();
        let (auth, mention) = self.is_authorized(user_id, &id_str);
        if auth {
            return (auth, mention);
        }

        // 如果當前頻道沒過，嘗試檢查是否為 Thread 並查找 Parent
        if let Ok(channel) = channel_id.to_channel(&ctx.http).await {
            if let Some(guild_channel) = channel.guild() {
                if let Some(parent_id) = guild_channel.parent_id {
                    return self.is_authorized(user_id, &parent_id.to_string());
                }
            }
        }

        (false, false)
    }

    pub fn create_token(&self, type_: &str, id: &str) -> Result<String> {
        let token: String = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(6)
            .map(char::from)
            .collect();

        let entry = PendingToken {
            token: token.clone(),
            type_: type_.to_string(),
            id: id.to_string(),
            expires_at: Utc::now() + Duration::minutes(5),
        };

        self.with_lock(
            self.pending_path.clone(),
            PendingStore::default(),
            |store| {
                // Cleanup expired tokens
                let now = Utc::now();
                store.tokens.retain(|_, v| v.expires_at > now);
                // Add new token
                store.tokens.insert(token.clone(), entry);
                Ok(())
            },
        )?;

        Ok(token)
    }

    pub fn redeem_token(&self, token: &str) -> Result<(String, String)> {
        // (type, id)
        let mut found_entry: Option<PendingToken> = None;

        // 1. Validate and Remove Token
        self.with_lock(
            self.pending_path.clone(),
            PendingStore::default(),
            |store| {
                let now = Utc::now();
                store.tokens.retain(|_, v| v.expires_at > now);

                if let Some(entry) = store.tokens.remove(token) {
                    found_entry = Some(entry);
                }
                Ok(())
            },
        )?;

        let entry = found_entry.ok_or_else(|| anyhow::anyhow!("Invalid or expired token"))?;

        // 2. Add to Registry
        self.with_lock(self.auth_path.clone(), Registry::default(), |reg| {
            let auth_entry = AuthEntry {
                authorized_at: Utc::now(),
                mention_only: entry.type_ == "channel", // Default true for channels
            };
            match entry.type_.as_str() {
                "user" => {
                    reg.users.insert(entry.id.clone(), auth_entry);
                }
                "channel" => {
                    reg.channels.insert(entry.id.clone(), auth_entry);
                }
                _ => {}
            }
            Ok(())
        })?;

        Ok((entry.type_, entry.id))
    }

    // New method: Toggle mention_only
    pub fn set_mention_only(&self, channel_id: &str, enable: bool) -> Result<()> {
        self.with_lock(self.auth_path.clone(), Registry::default(), |reg| {
            if let Some(entry) = reg.channels.get_mut(channel_id) {
                entry.mention_only = enable;
            } else {
                // If not authorized yet, maybe auto-authorize? No, fail.
                anyhow::bail!("Channel not authorized yet.");
            }
            Ok(())
        })?;
        Ok(())
    }
}
