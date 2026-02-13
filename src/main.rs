use serenity::async_trait;
use serenity::all::*;
use std::collections::HashMap;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};
use std::process::Command as StdCommand;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast, RwLock};
use std::fs;
use std::path::PathBuf;
mod auth;
use auth::AuthManager;
use std::time::Duration;
use tracing::{info, warn, error, Level};
use std::sync::atomic::{AtomicBool, Ordering};
use rust_embed::RustEmbed;
use directories::UserDirs;
use clap::{Parser, Subcommand};
use tokio::signal::unix::{signal, SignalKind};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the Discord bot
    Run,
    /// Manage systemd service
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
    /// Reload configuration for the running daemon
    Reload,
    /// Authorize a user or channel using a token
    Auth {
        /// The 6-character authentication token
        token: String,
    },
    /// Show version info
    Version,
}

#[derive(Subcommand)]
enum DaemonAction {
    /// Enable and start the systemd service (auto-start on boot)
    Enable,
    /// Stop and disable the systemd service
    Disable,
}

#[derive(RustEmbed)]
#[folder = "locales/"]
struct Asset;

#[derive(Deserialize, Clone)]
struct Config {
    discord_token: String,
    initial_prompt: Option<String>,
    debug_level: Option<String>,
    #[serde(default = "default_lang")]
    language: String,
}

#[derive(Clone)]
struct AppState {
    config: Arc<RwLock<Config>>,
    i18n: Arc<RwLock<I18n>>,
    config_path: PathBuf,
    auth: Arc<AuthManager>,
}

fn default_lang() -> String { "zh-TW".to_string() }

struct I18n {
    texts: Value,
}

impl I18n {
    fn new(lang: &str) -> Self {
        let path = format!("{}.json", lang);
        let content = if let Some(file) = Asset::get(&path) {
            std::str::from_utf8(file.data.as_ref()).expect("Invalid UTF-8 in locale").to_string()
        } else {
            // Last ditch fallback to English
            eprintln!("Warning: Locale {} not found, defaulting to en", lang);
            r#"{"processing": "Processing...", "api_error": "API Error", "user_aborted": "Aborted", "aborted_desc": "User aborted.", "pi_response": "Pi Response", "pi_working": "Thinking...", "wait": "Please wait...", "abort_sent": "Abort signal sent.", "loading_skill": "Loading skill {}...", "exec_success": "Success: {}", "exec_failed": "Failed: {}", "auto_retry": "🔄 **Auto-retry** ({}/{}) due to error..."}"#.to_string()
        };
        let texts = serde_json::from_str(&content).expect("Failed to parse locale");
        I18n { texts }
    }
    fn get(&self, key: &str) -> String {
        self.texts.get(key).and_then(|v| v.as_str()).unwrap_or(key).to_string()
    }
    fn get_arg(&self, key: &str, arg: &str) -> String {
        self.get(key).replace("{}", arg)
    }
    fn get_args<S: AsRef<str>>(&self, key: &str, args: &[S]) -> String {
        let mut s = self.get(key);
        for arg in args {
            s = s.replacen("{}", arg.as_ref(), 1);
        }
        s
    }
}

fn get_session_dir() -> PathBuf {
    let home = if let Some(user_dirs) = UserDirs::new() {
        user_dirs.home_dir().to_path_buf()
    } else {
        std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."))
    };
    home.join(".pi").join("discord-rs").join("sessions")
}

struct PiInstance {
    stdin: Arc<Mutex<ChildStdin>>,
    event_tx: broadcast::Sender<Value>,
    msg_buffer: Arc<Mutex<Vec<String>>>,
    is_processing: Arc<AtomicBool>,
    _child: tokio::process::Child, // Keep the child alive
}

impl PiInstance {
    async fn new(channel_id: u64, config: &Config) -> anyhow::Result<Arc<Self>> {
        let session_dir = get_session_dir();
        fs::create_dir_all(&session_dir)?;

        // Use PI_BINARY env var if set (from daemon), otherwise default to "pi"
        let pi_binary = std::env::var("PI_BINARY").unwrap_or_else(|_| "pi".to_string());
        let mut cmd = Command::new(pi_binary);
        cmd.arg("--mode").arg("rpc");
        
        let session_file = session_dir.join(format!("discord-rs-{}.jsonl", channel_id));
        cmd.arg("--session").arg(session_file);
        cmd.arg("--session-dir").arg(session_dir);
        
        let mut child = cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        info!("🚀 Started pi process for channel {}: {:?}", channel_id, cmd);

        let stdin_raw = child.stdin.take().ok_or_else(|| anyhow::anyhow!("Failed to open stdin"))?;
        let stdin = Arc::new(Mutex::new(stdin_raw));
        let stdout = child.stdout.take().ok_or_else(|| anyhow::anyhow!("Failed to open stdout"))?;
        let stderr = child.stderr.take().ok_or_else(|| anyhow::anyhow!("Failed to open stderr"))?;
        
        let (event_tx, _) = broadcast::channel(1000);
        let tx = event_tx.clone();

        // Task to log stderr
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            while let Ok(n) = reader.read_line(&mut line).await {
                if n == 0 { break; }
                info!("[PI-STDERR-{}]: {}", channel_id, line.trim());
                line.clear();
            }
        });

                // Task to parse stdout
        let tx_c = tx.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            while let Ok(n) = reader.read_line(&mut line).await {
                if n == 0 { 
                    info!("🔌 Pi process stdout closed for channel {}", channel_id);
                    let _ = tx_c.send(json!({"type": "error", "assistantMessageEvent": {"type": "error", "errorMessage": "Pi process exited unexpectedly."}}));
                    break; 
                }
                let trimmed = line.trim();
                if trimmed.is_empty() { continue; }
                
                if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
                    let _ = tx_c.send(val);
                } else {
                    info!("[PI-STDOUT-{}]: {}", channel_id, trimmed);
                }
                line.clear();
            }
        });

        let instance = Arc::new(PiInstance { 
            stdin, 
            event_tx, 
            msg_buffer: Arc::new(Mutex::new(Vec::new())), 
            is_processing: Arc::new(AtomicBool::new(false)),
            _child: child,
        });
        let mut rx = instance.event_tx.subscribe();
        
        // Initial setup
        instance.raw_call(json!({ "type": "set_session_name", "name": format!("discord-rs-{}", channel_id) })).await?;
        let id = instance.raw_call(json!({ "type": "get_state" })).await?;
        
        while let Ok(ev) = rx.recv().await {
            if ev["type"] == "response" && ev["id"] == id {
                if ev["data"]["messageCount"].as_u64().unwrap_or(0) == 0 {
                    if let Some(ref p) = config.initial_prompt { 
                        instance.raw_call(json!({ "type": "prompt", "message": p })).await?; 
                    }
                }
                break;
            }
            if ev["type"] == "error" {
                anyhow::bail!("Pi initialization error: {}", ev["assistantMessageEvent"]["errorMessage"]);
            }
        }
        
        Ok(instance)
    }
    async fn raw_call(&self, mut cmd: Value) -> anyhow::Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        cmd.as_object_mut().unwrap().insert("id".to_string(), json!(id));
        let mut stdin = self.stdin.lock().await;
        stdin.write_all((serde_json::to_string(&cmd)? + "\n").as_bytes()).await?;
        stdin.flush().await?;
        Ok(id)
    }
}

struct Handler {
    instances: Arc<RwLock<HashMap<u64, Arc<PiInstance>>>>,
    state: AppState,
}

#[derive(PartialEq, Clone, Debug)]
enum ExecStatus { Running, Success, Error(String), Aborted }

impl Handler {
    fn safe_truncate(s: &str, max: usize) -> String {
        let count = s.chars().count();
        if count > max {
            let skip = count - max;
            format!("...{}", s.chars().skip(skip).collect::<String>())
        } else { s.to_string() }
    }

    async fn start_loop(pi: Arc<PiInstance>, http: Arc<Http>, ch_id: ChannelId, state: AppState) {
        if pi.is_processing.swap(true, Ordering::SeqCst) { return; }
        tokio::spawn(async move {
            while pi.is_processing.load(Ordering::SeqCst) {
                let prompt = {
                    let mut b = pi.msg_buffer.lock().await;
                    if b.is_empty() { pi.is_processing.store(false, Ordering::SeqCst); break; }
                    let c = b.join("\n"); b.clear(); c
                };
                let mut rx = pi.event_tx.subscribe();
                let _ = pi.raw_call(json!({ "type": "prompt", "message": prompt })).await;
                let (processing_msg, _) = {
                    let i = state.i18n.read().await;
                    let c = state.config.read().await;
                    (i.get("processing"), c.initial_prompt.clone())
                };
                let mut discord_msg = ch_id.send_message(&http, CreateMessage::new().embed(CreateEmbed::new().title(processing_msg).color(0xFFA500)).allowed_mentions(CreateAllowedMentions::new().all_users(false))).await.unwrap();
                let (mut thinking, mut text, mut tool_info, mut status, mut last_upd) = (String::new(), String::new(), String::new(), ExecStatus::Running, std::time::Instant::now());
                
                let http_c = http.clone();
                let typing_task = tokio::spawn(async move {
                    loop {
                        let _ = ch_id.broadcast_typing(&http_c).await;
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                });

                while let Ok(ev) = rx.recv().await {
                    match ev["type"].as_str() {
                        Some("message_update") => {
                            let delta = &ev["assistantMessageEvent"];
                            if let Some(content) = delta.get("partial").and_then(|p| p.get("content")).and_then(|c| c.as_array()) {
                                thinking.clear();
                                text.clear();
                                for item in content {
                                    match item["type"].as_str() {
                                        Some("thinking") => thinking.push_str(item["thinking"].as_str().unwrap_or("")),
                                        Some("text") => text.push_str(item["text"].as_str().unwrap_or("")),
                                        _ => {}
                                    }
                                }
                            } else {
                                if delta["type"] == "thinking_delta" { thinking.push_str(delta["delta"].as_str().unwrap_or("")); }
                                if delta["type"] == "text_delta" { text.push_str(delta["delta"].as_str().unwrap_or("")); }
                            }
                            if delta["type"] == "error" {
                                let err_msg = delta["errorMessage"].as_str().unwrap_or("Unknown API error");
                                warn!("⚠️ [PATH: API_ERROR_EVENT] Quota or API issue: {}", err_msg);
                                status = if delta["reason"] == "aborted" { ExecStatus::Aborted } else { ExecStatus::Error(err_msg.to_string()) };
                            }
                        }
                        Some("tool_execution_start") => tool_info = format!("🛠️ **Executing:** `{}`", ev["toolName"].as_str().unwrap_or("tool")),
                        Some("tool_execution_end") => tool_info = String::new(),
                        Some("message_start") => {
                            if let Some(err) = ev["message"]["errorMessage"].as_str() {
                                warn!("⚠️ [PATH: MSG_START_ERROR] {}", err);
                                status = ExecStatus::Error(err.to_string());
                            }
                        }
                        Some("auto_retry_start") => {
                            let attempt = ev["attempt"].as_u64().unwrap_or(0);
                            let max = ev["maxAttempts"].as_u64().unwrap_or(0);
                            tool_info = state.i18n.read().await.get_args("auto_retry", &[&attempt.to_string(), &max.to_string()]);
                            // Reset text buffer on retry to avoid stale content
                            thinking.clear();
                            text.clear();
                        }
                        Some("agent_end") => { 
                            if let Some(err) = ev["errorMessage"].as_str() {
                                status = ExecStatus::Error(err.to_string());
                            } else if let Some(msgs) = ev["messages"].as_array() {
                                if let Some(last_msg) = msgs.last() {
                                    if let Some(err) = last_msg["errorMessage"].as_str() {
                                        status = ExecStatus::Error(err.to_string());
                                    } else if status == ExecStatus::Running {
                                        status = ExecStatus::Success;
                                    }
                                } else if status == ExecStatus::Running { status = ExecStatus::Success; }
                            } else if status == ExecStatus::Running { 
                                status = ExecStatus::Success; 
                            } 
                        }
                        Some("error") => {
                            let err_msg = ev["message"].as_str()
                                .or(ev["error"].as_str())
                                .unwrap_or("Unknown top-level error");
                            warn!("⚠️ [PATH: TOP_LEVEL_ERROR] {}", err_msg);
                            status = ExecStatus::Error(err_msg.to_string());
                        }
                        _ => {}
                    }
                    if last_upd.elapsed() >= Duration::from_secs(2) || status != ExecStatus::Running {
                        let mut embed = CreateEmbed::new();
                        let mut desc = String::new();
                        let i18n = state.i18n.read().await;
                        
                        if !thinking.is_empty() {
                            let thinking_txt = format!("🧠 {}", Self::safe_truncate(&thinking, 500));
                            // Format: Start with "> ", replace all internal newlines with "\n> "
                            desc.push_str("> ");
                            desc.push_str(&thinking_txt.replace("\n", "\n> "));
                            // Ensure there's a trailing newline after the quote block to prevent it from "sticky"
                            if !desc.ends_with('\n') {
                                desc.push_str("\n");
                            }
                            desc.push_str("\n");
                        }
                        match status {
                            ExecStatus::Error(ref e) => {
                                info!("🚩 [PATH: DISPLAY_ERROR] Rendering error to Discord: {}", e);
                                embed = embed.title(i18n.get("api_error")).color(0xff0000);
                                if !text.is_empty() { desc.push_str(&format!("{}\n\n", text)); }
                                desc.push_str(&format!("❌ **Error:** {}", e));
                            }
                            ExecStatus::Aborted => {
                                embed = embed.title(i18n.get("user_aborted")).color(0xff0000);
                                if !text.is_empty() { desc.push_str(&format!("{}\n\n", text)); }
                                desc.push_str(&format!("⚠️ {}", i18n.get("aborted_desc")));
                            }
                            ExecStatus::Success => {
                                embed = embed.title(i18n.get("pi_response")).color(0x00ff00);
                                desc.push_str(&text);
                            }
                            ExecStatus::Running => {
                                embed = embed.title(i18n.get("pi_working")).color(0xFFA500);
                                if !tool_info.is_empty() { desc.push_str(&format!("{}\n\n", tool_info)); }
                                desc.push_str(&text);
                            }
                        }
                        if desc.is_empty() { desc = i18n.get("wait"); }
                        let _ = discord_msg.edit(&http, EditMessage::new().embed(embed.description(Self::safe_truncate(&desc, 4000)))).await;
                        last_upd = std::time::Instant::now();
                        if status != ExecStatus::Running { 
                            typing_task.abort();
                            break; 
                        }
                    }
                }
                if !typing_task.is_finished() { typing_task.abort(); }
            }
        });
    }
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        info!("✅ Connected as {}!", ready.user.name);
        let http = ctx.http.clone();
        
        // Cleanup: Clear all guild-specific commands to avoid duplicates with global commands
        for guild in &ready.guilds {
            if let Err(e) = guild.id.set_commands(&http, vec![]).await {
                warn!("Failed to clear commands for guild {}: {}", guild.id, e);
            }
        }

        let cfg = self.state.config.read().await.clone();
        tokio::spawn(async move {
            let mut model_choices = Vec::new();
            if let Ok(pi) = PiInstance::new(0, &cfg).await {
                let mut rx = pi.event_tx.subscribe();
                if let Ok(id) = pi.raw_call(json!({ "type": "get_available_models" })).await {
                    while let Ok(event) = rx.recv().await {
                        if event["type"] == "response" && event["id"] == id {
                            if let Some(models) = event["data"]["models"].as_array() {
                                for m in models {
                                    if model_choices.len() >= 25 { break; }
                                    let label = format!("{}/{}", m["provider"].as_str().unwrap_or("?"), m["id"].as_str().unwrap_or("?"));
                                    model_choices.push(json!({ "name": label, "value": label }));
                                }
                            }
                            break;
                        }
                    }
                }
            }
            let model_opt = model_choices.iter().fold(CreateCommandOption::new(CommandOptionType::String, "id", "Select model").required(true), |o, c| o.add_string_choice(c["name"].as_str().unwrap(), c["value"].as_str().unwrap()));
            let discord_cmds = vec![
                CreateCommand::new("model").description("Switch model").add_option(model_opt),
                CreateCommand::new("thinking").description("Set thinking level").add_option(CreateCommandOption::new(CommandOptionType::String, "level", "Level").required(true).add_string_choice("off", "off").add_string_choice("minimal", "minimal").add_string_choice("low", "low").add_string_choice("medium", "medium").add_string_choice("high", "high").add_string_choice("xhigh", "xhigh")),
                CreateCommand::new("compact").description("Compact history"),
                CreateCommand::new("clear").description("Clear session"),
                CreateCommand::new("abort").description("Abort operation"),
                CreateCommand::new("skill").description("Use a skill").add_option(CreateCommandOption::new(CommandOptionType::String, "name", "Skill").required(true)),
                CreateCommand::new("mention_only").description("Toggle mention-only mode").add_option(CreateCommandOption::new(CommandOptionType::Boolean, "enable", "Enable?").required(true))
            ];
            let _ = serenity::all::Command::set_global_commands(&http, discord_cmds).await;
        });
    }

    async fn message(&self, ctx: Context, msg: Message) {
        if msg.author.bot { return; }

        let user_id = msg.author.id.to_string();
        let channel_id_str = msg.channel_id.to_string();
        
        // Check Authorization
        let (is_auth, mention_only) = self.state.auth.is_authorized(&user_id, &channel_id_str);
        
        if !is_auth {
            let is_dm = msg.guild_id.is_none();
            let mentioned = msg.mentions_me(&ctx).await.unwrap_or(false);
            
            if is_dm || mentioned {
                match self.state.auth.create_token(
                    if is_dm { "user" } else { "channel" },
                    if is_dm { &user_id } else { &channel_id_str }
                ) {
                    Ok(token) => {
                        let _ = msg.reply(&ctx.http, format!("🔒 Authentication required!\nRun this command on your server to authorize:\n`discord-rs auth {}`\n(Expires in 5 mins)", token)).await;
                    },
                    Err(e) => {
                        warn!("Failed to generate auth token: {}", e);
                    }
                }
            }
            return;
        }

        // Check Mention Only
        if mention_only && !msg.mentions_me(&ctx).await.unwrap_or(false) && msg.guild_id.is_some() {
            return;
        }

        let channel_id = msg.channel_id.get();
        let pi = {
            let instances = self.instances.read().await;
            if let Some(pi) = instances.get(&channel_id) { pi.clone() }
            else {
                drop(instances);
                let mut instances = self.instances.write().await;
                if let Some(pi) = instances.get(&channel_id) { pi.clone() }
                else {
                    match PiInstance::new(channel_id, &*self.state.config.read().await).await {
                        Ok(pi) => {
                            instances.insert(channel_id, pi.clone());
                            pi
                        },
                        Err(e) => {
                            error!("❌ Failed to start Pi instance for channel {}: {}", channel_id, e);
                            let _ = msg.reply(&ctx.http, format!("❌ **System Error**: Failed to initialize Pi process.\nDetails: `{}`", e)).await;
                            return;
                        }
                    }
                }
            }
        };
        pi.msg_buffer.lock().await.push(if msg.content.starts_with("!") { &msg.content[1..] } else { &msg.content }.to_string());
        Self::start_loop(pi, ctx.http.clone(), msg.channel_id, self.state.clone()).await;
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        if let Interaction::Command(command) = interaction {
            let cmd_name = command.data.name.clone();
            let channel_id_u64 = command.channel_id.get();
            info!("🔔 Received slash command: {}", cmd_name);

            // 1. Handle commands that don't need a Pi instance OR handle their own deferring
            if cmd_name == "abort" {
                let pi_opt = { self.instances.read().await.get(&channel_id_u64).cloned() };
                if let Some(pi) = pi_opt {
                    let _ = pi.raw_call(json!({ "type": "abort" })).await;
                    pi.msg_buffer.lock().await.clear();
                    let _ = command.create_response(&ctx.http, CreateInteractionResponse::Message(CreateInteractionResponseMessage::new().content(self.state.i18n.read().await.get("abort_sent")).ephemeral(true))).await;
                } else {
                    let _ = command.create_response(&ctx.http, CreateInteractionResponse::Message(CreateInteractionResponseMessage::new().content("❌ No active session to abort.").ephemeral(true))).await;
                }
                return;
            }

            // 2. Defer all other commands
            if let Err(e) = command.defer_ephemeral(&ctx.http).await {
                error!("❌ Failed to defer interaction for {}: {}", cmd_name, e);
                return;
            }

            // 3. Handle commands that work WITHOUT a Pi instance
            if cmd_name == "mention_only" {
                let enable = command.data.options.iter().find(|o| o.name == "enable").and_then(|o| o.value.as_bool()).unwrap_or(true);
                let ch_id = command.channel_id.to_string();
                let msg = match self.state.auth.set_mention_only(&ch_id, enable) {
                    Ok(_) => format!("✅ Mention-only mode: **{}**", enable),
                    Err(_) => "❌ Channel not authorized.".to_string(),
                };
                let _ = command.edit_response(&ctx.http, EditInteractionResponse::new().content(msg)).await;
                return;
            }

            if cmd_name == "clear" {
                let mut instances = self.instances.write().await;
                instances.remove(&channel_id_u64); // Drops instance and kills process
                
                let session_file = get_session_dir().join(format!("discord-rs-{}.jsonl", channel_id_u64));
                if session_file.exists() {
                    let _ = fs::remove_file(session_file);
                }
                
                let _ = command.edit_response(&ctx.http, EditInteractionResponse::new().content(self.state.i18n.read().await.get_arg("exec_success", "clear"))).await;
                return;
            }

            // 4. Handle commands that REQUIRE a Pi instance
            let pi_opt = { self.instances.read().await.get(&channel_id_u64).cloned() };
            if let Some(pi) = pi_opt {
                let req_id = match cmd_name.as_str() {
                    "model" => {
                        let id_val = command.data.options.iter().find(|o| o.name == "id").and_then(|o| o.value.as_str()).unwrap_or("");
                        if let Some((p, mid)) = id_val.split_once('/') { Some(pi.raw_call(json!({ "type": "set_model", "provider": p, "modelId": mid })).await.unwrap()) } else { None }
                    }
                    "thinking" => {
                        let lvl = command.data.options.iter().find(|o| o.name == "level").and_then(|o| o.value.as_str()).unwrap_or("medium");
                        Some(pi.raw_call(json!({ "type": "set_thinking_level", "level": lvl })).await.unwrap())
                    }
                    "compact" => Some(pi.raw_call(json!({ "type": "compact" })).await.unwrap()),
                    "skill" => {
                        let n = command.data.options.iter().find(|o| o.name == "name").and_then(|o| o.value.as_str()).unwrap_or("");
                        pi.msg_buffer.lock().await.push(format!("/skill:{}", n));
                        Self::start_loop(pi.clone(), ctx.http.clone(), command.channel_id, self.state.clone()).await;
                        let _ = command.edit_response(&ctx.http, EditInteractionResponse::new().content(self.state.i18n.read().await.get_arg("loading_skill", n))).await;
                        return;
                    }
                    _ => None,
                };

                if let Some(rid) = req_id {
                    let mut rx = pi.event_tx.subscribe();
                    let http = ctx.http.clone();
                    let cmd_clone = command.clone();
                    let state = self.state.clone();
                    let pi_c = pi.clone();
                    let initial_prompt = self.state.config.read().await.initial_prompt.clone();
                    let cmd_name_c = cmd_name.clone();
                    
                    tokio::spawn(async move {
                        while let Ok(event) = rx.recv().await {
                            if event["type"] == "response" && event["id"] == rid {
                                let success = event["success"].as_bool().unwrap_or(false);
                                let c = if success { state.i18n.read().await.get_arg("exec_success", &cmd_name_c) } else { state.i18n.read().await.get_arg("exec_failed", &cmd_name_c) };
                                let _ = cmd_clone.edit_response(&http, EditInteractionResponse::new().content(c)).await;
                                
                                // If clear was successful, re-send the initial prompt if it exists
                                if success && cmd_name_c == "clear" {
                                    if let Some(p) = initial_prompt {
                                        let _ = pi_c.raw_call(json!({ "type": "prompt", "message": p })).await;
                                    }
                                }
                                break;
                            }
                        }
                    });
                }
            } else {
                let _ = command.edit_response(&ctx.http, EditInteractionResponse::new().content("❌ No active session in this channel. Send a message first.")).await;
            }
        }
    }
}

async fn run_bot() -> anyhow::Result<()> {
    // Resolve config path: ~/.pi/discord-rs/config.toml
    let user_dirs = UserDirs::new().ok_or_else(|| anyhow::anyhow!("Could not find user home directory"))?;
    let config_dir = user_dirs.home_dir().join(".pi").join("discord-rs");
    let config_path = config_dir.join("config.toml");

    // Check if config exists, if not create default and exit
    if !config_path.exists() {
        fs::create_dir_all(&config_dir)?;
        let default_config = r#"discord_token = "YOUR_DISCORD_TOKEN_HERE"
# initial_prompt = "你是一個助手，請用台灣繁體中文回覆。"
debug_level = "INFO"
language = "zh-TW"
"#;
        fs::write(&config_path, default_config)?;
        eprintln!("⚠️  Configuration file not found.");
        eprintln!("✅  Created default configuration at: {}", config_path.display());
        eprintln!("👉  Please edit this file with your Discord token before running again.");
        std::process::exit(1);
    }

    println!("Loading config from: {}", config_path.display());
    let config_str = fs::read_to_string(&config_path)?;
    let config: Config = toml::from_str(&config_str)?;
    
    let log_level = match config.debug_level.as_deref() { Some("DEBUG") => Level::DEBUG, _ => Level::INFO };
    tracing_subscriber::fmt().with_max_level(log_level).init();
    
    let i18n = Arc::new(RwLock::new(I18n::new(&config.language)));
    let config = Arc::new(RwLock::new(config));
    let auth = Arc::new(AuthManager::new());
    let state = AppState { config: config.clone(), i18n: i18n.clone(), config_path: config_path.clone(), auth: auth.clone() };
    
    // Spawn signal handler
    let state_c = state.clone();
    let config_path_c = config_path.clone(); // Capture path
    tokio::spawn(async move {
        let mut sighup = match signal(SignalKind::hangup()) {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to register SIGHUP handler: {}", e);
                return;
            }
        };
        loop {
            sighup.recv().await;
            info!("🔄 Received SIGHUP, reloading configuration...");
            
            match fs::read_to_string(&config_path_c) {
                Ok(content) => {
                    match toml::from_str::<Config>(&content) {
                        Ok(new_cfg) => {
                            // Update Config
                            let mut c_guard = state_c.config.write().await;
                            let old_lang = c_guard.language.clone();
                            *c_guard = new_cfg.clone();
                            drop(c_guard); // Drop lock early
                            
                            // Reload I18n if needed
                            if old_lang != new_cfg.language {
                                let mut i_guard = state_c.i18n.write().await;
                                *i_guard = I18n::new(&new_cfg.language);
                                info!("🌐 Language switched to: {}", new_cfg.language);
                            }
                            info!("✅ Configuration reloaded successfully!");
                        }
                        Err(e) => error!("❌ Failed to parse config on reload: {}", e),
                    }
                }
                Err(e) => error!("❌ Failed to read config file on reload: {}", e),
            }
        }
    });

    let handler = Handler { instances: Arc::new(RwLock::new(HashMap::new())), state: state.clone() };
    let token = state.config.read().await.discord_token.clone();
    let mut client = Client::builder(&token, GatewayIntents::GUILD_MESSAGES | GatewayIntents::MESSAGE_CONTENT | GatewayIntents::GUILDS | GatewayIntents::DIRECT_MESSAGES).event_handler(handler).await?;
    client.start().await?;
    Ok(())
}

fn manage_daemon(action: DaemonAction) -> anyhow::Result<()> {
    let service_name = "discord-rs";
    let user_dirs = UserDirs::new().ok_or_else(|| anyhow::anyhow!("Could not find user home directory"))?;
    let systemd_dir = user_dirs.home_dir().join(".config/systemd/user");
    let service_file = systemd_dir.join(format!("{}.service", service_name));

    match action {
        DaemonAction::Enable => {
            fs::create_dir_all(&systemd_dir)?;
            let exe_path = std::env::current_exe()?;
            
            // Resolve absolute path for 'pi' to ensure robustness in systemd environment
            let pi_binary = if let Ok(output) = StdCommand::new("sh").arg("-c").arg("which pi").output() {
                if output.status.success() {
                    String::from_utf8_lossy(&output.stdout).trim().to_string()
                } else {
                    "pi".to_string()
                }
            } else {
                "pi".to_string()
            };

            // Capture current PATH to ensure 'node' and other tools can be found in systemd
            let current_path = std::env::var("PATH").unwrap_or_default();

            let content = format!(r#"[Unit]
Description=Pi Discord RS
After=network.target

[Service]
Type=simple
ExecStart={} run
Environment="PI_BINARY={}"
Environment="PATH={}"
Restart=on-failure
RestartSec=5s

[Install]
WantedBy=default.target
"#, exe_path.display(), pi_binary, current_path);
            
            fs::write(&service_file, content)?;
            println!("Created systemd service file at: {}", service_file.display());
            
            StdCommand::new("systemctl").arg("--user").arg("daemon-reload").status()?;
            StdCommand::new("systemctl").arg("--user").arg("enable").arg(service_name).status()?;
            StdCommand::new("systemctl").arg("--user").arg("start").arg(service_name).status()?;
            println!("✅ Service enabled and started!");
        },
        DaemonAction::Disable => {
            let _ = StdCommand::new("systemctl").arg("--user").arg("stop").arg(service_name).status();
            let _ = StdCommand::new("systemctl").arg("--user").arg("disable").arg(service_name).status();
            if service_file.exists() {
                fs::remove_file(service_file)?;
            }
            let _ = StdCommand::new("systemctl").arg("--user").arg("daemon-reload").status();
            println!("🛑 Service disabled and removed.");
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Run) => run_bot().await?,
        Some(Commands::Daemon { action }) => manage_daemon(action)?,
        Some(Commands::Reload) => {
            let res = StdCommand::new("systemctl").arg("--user").arg("kill").arg("-s").arg("HUP").arg("discord-rs").status();
            match res {
                Ok(status) if status.success() => println!("✅ Reload signal sent successfully."),
                _ => eprintln!("❌ Failed to send reload signal. Is the daemon running?"),
            }
        }
        Some(Commands::Auth { token }) => {
            let manager = AuthManager::new();
            match manager.redeem_token(&token) {
                Ok((type_, id)) => {
                    println!("✅ Successfully authorized {} {}.", type_, id);
                },
                Err(e) => eprintln!("❌ Authorization failed: {}", e),
            }
        }
        Some(Commands::Version) => {
            println!("discord-rs v{}", env!("CARGO_PKG_VERSION"));
        },
        None => {
            use clap::CommandFactory;
            Cli::command().print_help()?;
        }
    }
    Ok(())
}
