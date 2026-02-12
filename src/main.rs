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
use std::time::Duration;
use tracing::{info, debug, Level};
use std::sync::atomic::{AtomicBool, Ordering};
use rust_embed::RustEmbed;
use directories::UserDirs;
use clap::{Parser, Subcommand};

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
            // Fallback to reading from filesystem if not found embedded (useful for dev)
            if let Ok(c) = fs::read_to_string(format!("locales/{}", path)) {
                c
            } else {
                eprintln!("Warning: Locale {} not found, defaulting to en", lang);
                r#"{"processing": "Processing...", "api_error": "API Error", "user_aborted": "Aborted", "aborted_desc": "User aborted.", "pi_response": "Pi Response", "pi_working": "Thinking...", "wait": "Please wait...", "abort_sent": "Abort signal sent.", "loading_skill": "Loading skill {}...", "exec_success": "Success: {}", "exec_failed": "Failed: {}"}"#.to_string()
            }
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
}

struct PiInstance {
    stdin: Arc<Mutex<ChildStdin>>,
    event_tx: broadcast::Sender<Value>,
    msg_buffer: Arc<Mutex<Vec<String>>>,
    is_processing: Arc<AtomicBool>,
}

impl PiInstance {
    async fn new(channel_id: u64, config: &Config) -> anyhow::Result<Arc<Self>> {
        // Hardcoded session directory to ~/.pi/discord-rs/sessions
        let session_dir = if let Some(user_dirs) = UserDirs::new() {
             user_dirs.home_dir().join(".pi").join("discord-rs").join("sessions")
        } else {
             PathBuf::from("sessions")
        };
        fs::create_dir_all(&session_dir)?;

        let mut cmd = Command::new("pi");
        cmd.arg("--mode").arg("rpc");
        
        let session_file = session_dir.join(format!("discord-rs-{}.jsonl", channel_id));
        cmd.arg("--session").arg(session_file);
        cmd.arg("--session-dir").arg(session_dir);
        
        let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).spawn()?;
        let stdin = Arc::new(Mutex::new(child.stdin.take().unwrap()));
        let stdout = child.stdout.take().unwrap();
        let (event_tx, _) = broadcast::channel(1000);
        let tx = event_tx.clone();
        let mut reader = BufReader::new(stdout);
        tokio::spawn(async move {
            let mut line = String::new();
            while let Ok(n) = reader.read_line(&mut line).await {
                if n == 0 { break; }
                debug!("[RAW-{}]: {}", channel_id, line.trim());
                if let Ok(val) = serde_json::from_str::<Value>(&line) { let _ = tx.send(val); }
                line.clear();
            }
        });
        let instance = Arc::new(PiInstance { stdin, event_tx, msg_buffer: Arc::new(Mutex::new(Vec::new())), is_processing: Arc::new(AtomicBool::new(false)) });
        let mut rx = instance.event_tx.subscribe();
        instance.raw_call(json!({ "type": "set_session_name", "name": format!("discord-rs-{}", channel_id) })).await?;
        let id = instance.raw_call(json!({ "type": "get_state" })).await?;
        while let Ok(ev) = rx.recv().await {
            if ev["type"] == "response" && ev["id"] == id {
                if ev["data"]["messageCount"].as_u64().unwrap_or(0) == 0 {
                    if let Some(ref p) = config.initial_prompt { instance.raw_call(json!({ "type": "prompt", "message": p })).await?; }
                }
                break;
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
    config: Config,
    i18n: Arc<I18n>,
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

    fn smallify(s: &str) -> String {
        format!(">>> *{}*\n", s)
    }

    async fn start_loop(pi: Arc<PiInstance>, http: Arc<Http>, ch_id: ChannelId, i18n: Arc<I18n>) {
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
                let mut discord_msg = ch_id.send_message(&http, CreateMessage::new().embed(CreateEmbed::new().title(i18n.get("processing")).color(0xFFA500)).allowed_mentions(CreateAllowedMentions::new().all_users(false))).await.unwrap();
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
                            if delta["type"] == "error" { status = if delta["reason"] == "aborted" { ExecStatus::Aborted } else { ExecStatus::Error(delta["errorMessage"].as_str().unwrap_or("").to_string()) }; }
                        }
                        Some("tool_execution_start") => tool_info = format!("🛠️ **Executing:** `{}`", ev["toolName"].as_str().unwrap_or("tool")),
                        Some("tool_execution_end") => tool_info = String::new(),
                        Some("agent_end") => { if status == ExecStatus::Running { status = ExecStatus::Success; } }
                        _ => {}
                    }
                    if last_upd.elapsed() >= Duration::from_secs(2) || status != ExecStatus::Running {
                        let mut embed = CreateEmbed::new();
                        let mut desc = String::new();
                        if !thinking.is_empty() {
                            desc.push_str(&Self::smallify(&format!("🧠 {}", Self::safe_truncate(&thinking, 500))));
                            desc.push_str("\n");
                        }
                        match status {
                            ExecStatus::Error(ref e) => { embed = embed.title(i18n.get("api_error")).color(0xff0000); desc.push_str(&format!("**Error:** {}", e)); }
                            ExecStatus::Aborted => { embed = embed.title(i18n.get("user_aborted")).color(0xff0000); desc.push_str(&i18n.get("aborted_desc")); }
                            ExecStatus::Success => { embed = embed.title(i18n.get("pi_response")).color(0x00ff00); desc.push_str(&text); }
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
        let cfg = self.config.clone();
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
                CreateCommand::new("skill").description("Use a skill").add_option(CreateCommandOption::new(CommandOptionType::String, "name", "Skill").required(true))
            ];
            let _ = serenity::all::Command::set_global_commands(&http, discord_cmds).await;
        });
    }

    async fn message(&self, ctx: Context, msg: Message) {
        if msg.author.bot { return; }
        let channel_id = msg.channel_id.get();
        let pi = {
            let instances = self.instances.read().await;
            if let Some(pi) = instances.get(&channel_id) { pi.clone() }
            else {
                drop(instances);
                let mut instances = self.instances.write().await;
                if let Some(pi) = instances.get(&channel_id) { pi.clone() }
                else {
                    let pi = PiInstance::new(channel_id, &self.config).await.unwrap();
                    instances.insert(channel_id, pi.clone());
                    pi
                }
            }
        };
        pi.msg_buffer.lock().await.push(if msg.content.starts_with("!") { &msg.content[1..] } else { &msg.content }.to_string());
        Self::start_loop(pi, ctx.http.clone(), msg.channel_id, self.i18n.clone()).await;
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        if let Interaction::Command(command) = interaction {
            let channel_id = command.channel_id.get();
            let pi = { self.instances.read().await.get(&channel_id).cloned() };
            if let Some(pi) = pi {
                let cmd_name = command.data.name.clone();
                if cmd_name == "abort" {
                    let _ = pi.raw_call(json!({ "type": "abort" })).await;
                    pi.msg_buffer.lock().await.clear();
                    let _ = command.create_response(&ctx.http, CreateInteractionResponse::Message(CreateInteractionResponseMessage::new().content(self.i18n.get("abort_sent")).ephemeral(true))).await;
                    return;
                }
                let _ = command.defer_ephemeral(&ctx.http).await;
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
                    "clear" => Some(pi.raw_call(json!({ "type": "new_session" })).await.unwrap()),
                    "skill" => {
                        let n = command.data.options.iter().find(|o| o.name == "name").and_then(|o| o.value.as_str()).unwrap_or("");
                        pi.msg_buffer.lock().await.push(format!("/skill:{}", n));
                        Self::start_loop(pi.clone(), ctx.http.clone(), command.channel_id, self.i18n.clone()).await;
                        let _ = command.edit_response(&ctx.http, EditInteractionResponse::new().content(self.i18n.get_arg("loading_skill", n))).await;
                        return;
                    }
                    _ => None,
                };
                if let Some(rid) = req_id {
                    let mut rx = pi.event_tx.subscribe();
                    let http = ctx.http.clone();
                    let cmd_clone = command.clone();
                    let i18n = self.i18n.clone();
                    tokio::spawn(async move {
                        while let Ok(event) = rx.recv().await {
                            if event["type"] == "response" && event["id"] == rid {
                                let c = if event["success"].as_bool().unwrap_or(false) { i18n.get_arg("exec_success", &cmd_name) } else { i18n.get_arg("exec_failed", &cmd_name) };
                                let _ = cmd_clone.edit_response(&http, EditInteractionResponse::new().content(c)).await;
                                break;
                            }
                        }
                    });
                }
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
    
    let i18n = Arc::new(I18n::new(&config.language));
    let handler = Handler { instances: Arc::new(RwLock::new(HashMap::new())), config: config.clone(), i18n };
    let mut client = Client::builder(&config.discord_token, GatewayIntents::GUILD_MESSAGES | GatewayIntents::MESSAGE_CONTENT | GatewayIntents::GUILDS).event_handler(handler).await?;
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
            let content = format!(r#"[Unit]
Description=Pi Discord RS
After=network.target

[Service]
Type=simple
ExecStart={} run
Restart=on-failure
RestartSec=5s

[Install]
WantedBy=default.target
"#, exe_path.display());
            fs::write(&service_file, content)?;
            println!("Created systemd service file at: {}", service_file.display());
            
            StdCommand::new("systemctl").arg("--user").arg("daemon-reload").status()?;
            StdCommand::new("systemctl").arg("--user").arg("enable").arg(service_name).status()?;
            StdCommand::new("systemctl").arg("--user").arg("start").arg(service_name).status()?;
            println!("✅ Service enabled and started!");
        },
        DaemonAction::Disable => {
            StdCommand::new("systemctl").arg("--user").arg("stop").arg(service_name).status()?;
            StdCommand::new("systemctl").arg("--user").arg("disable").arg(service_name).status()?;
            if service_file.exists() {
                fs::remove_file(service_file)?;
            }
            StdCommand::new("systemctl").arg("--user").arg("daemon-reload").status()?;
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
        None => {
            use clap::CommandFactory;
            Cli::command().print_help()?;
        }
    }
    Ok(())
}
