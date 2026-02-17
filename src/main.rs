use agent::{AgentEvent, AiAgent, ContentType};
use clap::{Parser, Subcommand};
use rust_embed::RustEmbed;
use serenity::all::{
    Context, CreateEmbed, CreateMessage, EditMessage, EventHandler,
    GatewayIntents, Interaction, Message, Ready,
};
use serenity::async_trait;
use serenity::Client;
use std::process::Command as StdCommand;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, error, info, Level};

mod agent;
mod auth;
mod commands;
mod composer;
mod config;
mod migrate;
mod session;

use auth::AuthManager;
use commands::agent::{handle_button, ChannelConfig};
use composer::{Block, BlockType, EmbedComposer};
use config::Config;
use session::SessionManager;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    Run,
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
    Reload,
    Auth {
        token: String,
    },
    Version,
}

#[derive(Subcommand)]
enum DaemonAction {
    Enable,
    Disable,
}

#[derive(RustEmbed)]
#[folder = "locales/"]
struct Asset;

#[derive(RustEmbed)]
#[folder = "prompts/"]
struct DefaultPrompts;

#[derive(Clone)]
struct AppState {
    config: Arc<Config>,
    session_manager: Arc<SessionManager>,
    auth: Arc<AuthManager>,
    i18n: Arc<RwLock<I18n>>,
    backend_manager: Arc<agent::manager::BackendManager>,
}

struct I18n {
    texts: serde_json::Value,
}

impl I18n {
    fn new(lang: &str) -> Self {
        let path = format!("{}.json", lang);
        let content = if let Some(file) = Asset::get(&path) {
            std::str::from_utf8(file.data.as_ref())
                .expect("UTF-8")
                .to_string()
        } else {
            r#"{"processing": "...", "wait": "..."}"#.to_string()
        };
        I18n {
            texts: serde_json::from_str(&content).expect("JSON"),
        }
    }
    fn get(&self, key: &str) -> String {
        self.texts
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or(key)
            .to_string()
    }
}

fn load_all_prompts() -> String {
    let prompts_dir = migrate::get_prompts_dir();
    let _ = std::fs::create_dir_all(&prompts_dir);
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&prompts_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Ok(content) = std::fs::read_to_string(&path) {
                files.push((path.file_name().unwrap().to_owned(), content));
            }
        }
    }
    if files.is_empty() {
        for file in DefaultPrompts::iter() {
            if let Some(content) = DefaultPrompts::get(&file) {
                let s = std::str::from_utf8(content.data.as_ref()).unwrap();
                let _ = std::fs::write(prompts_dir.join(file.as_ref()), s);
                files.push((file.as_ref().into(), s.to_string()));
            }
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
        .into_iter()
        .map(|(_, c)| c)
        .collect::<Vec<_>>()
        .join("\n\n")
}

struct Handler {
    state: AppState,
}

#[derive(Clone, Debug, PartialEq)]
enum ExecStatus {
    Running,
    Success,
    Error(String),
}

impl Handler {
    async fn start_agent_loop(
        agent: Arc<dyn AiAgent>,
        http: Arc<serenity::http::Http>,
        channel_id: serenity::model::id::ChannelId,
        state: AppState,
        initial_message: Option<String>,
        is_brand_new: bool,
    ) {
        let i18n = state.i18n.read().await;
        let processing_msg = i18n.get("processing");
        drop(i18n);

        let discord_msg = match channel_id
            .send_message(
                &http,
                CreateMessage::new()
                    .embed(CreateEmbed::new().title(&processing_msg).color(0xFFA500)),
            )
            .await
        {
            Ok(m) => m,
            Err(e) => {
                error!("Failed to send: {}", e);
                return;
            }
        };

        let composer: Arc<Mutex<EmbedComposer>> = Arc::new(Mutex::new(EmbedComposer::new(3900)));
        let status: Arc<Mutex<ExecStatus>> = Arc::new(Mutex::new(ExecStatus::Running));

        if let Some(msg) = initial_message {
            let mut final_msg = msg;
            if is_brand_new {
                let prompts = load_all_prompts();
                if !prompts.is_empty() {
                    final_msg = format!("{}\n\n{}", prompts, final_msg);
                }
            }
            let agent_for_prompt = Arc::clone(&agent);
            let status_for_prompt = Arc::clone(&status);
            let composer_for_prompt = Arc::clone(&composer);
            tokio::spawn(async move {
                if let Err(e) = agent_for_prompt.prompt(&final_msg).await {
                    let mut s = status_for_prompt.lock().await;
                    let comp = composer_for_prompt.lock().await;
                    
                    // [核心修復]: 只有在目前還是 Running 且 Composer 完全沒收到內容時才設置 Error。
                    // 如果 Composer 已經有內容，代表後端已經收到請求並在透過 SSE 吐字，
                    // 此時 POST 的超時或失敗只是網路層的抖動，不應終止渲染。
                    if *s == ExecStatus::Running {
                        if comp.blocks.is_empty() {
                            *s = ExecStatus::Error(e.to_string());
                        } else {
                            info!("⚠️ POST prompt reported error: {}, but SSE stream is active. Continuing...", e);
                        }
                    }
                }
            });
        }

        let typing_http = http.clone();
        let typing_task = tokio::spawn(async move {
            loop {
                let _ = channel_id.broadcast_typing(&typing_http).await;
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        });

        // --- 任務 A: Render 循環 (心跳更新 Discord) ---
        let render_status = Arc::clone(&status);
        let render_composer = Arc::clone(&composer);
        let render_http = http.clone();
        let mut render_msg = discord_msg.clone();
        let render_i18n = Arc::clone(&state.i18n);
        let render_channel_id = channel_id;

        let render_task = tokio::spawn(async move {
            let mut last_content = String::new();
            let mut last_status = ExecStatus::Running;
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

                let (current_status, desc) = {
                    let c = render_composer.lock().await;
                    let s = render_status.lock().await;
                    (s.clone(), c.render())
                };

                if desc != last_content || current_status != last_status {
                    let mut embed = CreateEmbed::new();
                    let i18n = render_i18n.read().await;

                    match &current_status {
                        ExecStatus::Error(e) => {
                            embed = embed
                                .title(i18n.get("api_error"))
                                .color(0xff0000)
                                .description(format!("{}\n\n❌ **錯誤:** {}", desc, e));
                        }
                        ExecStatus::Success => {
                            embed = embed
                                .title(i18n.get("pi_response"))
                                .color(0x00ff00)
                                .description(if desc.is_empty() {
                                    i18n.get("done")
                                } else {
                                    desc.clone()
                                });
                        }
                        ExecStatus::Running => {
                            embed = embed
                                .title(i18n.get("pi_working"))
                                .color(0xFFA500)
                                .description(if desc.is_empty() {
                                    i18n.get("wait")
                                } else {
                                    desc.clone()
                                });
                        }
                    }

                    if let Err(e) = render_msg
                        .edit(&render_http, EditMessage::new().embed(embed))
                        .await
                    {
                        error!("❌ Render failed to edit message: {}", e);
                    } else {
                        info!(
                            "📢 [EMBED-UPDATE-{}]: status={:?}, len={}",
                            render_channel_id,
                            current_status,
                            desc.len()
                        );
                        last_content = desc;
                        last_status = current_status.clone();
                    }
                }

                if current_status != ExecStatus::Running {
                    // 為了確保最後一刻的內容也被渲染，我們在狀態變更後再跑一輪
                    break;
                }
            }
        });

        // --- 任務 B: Writer 任務 (極速吸收事件) ---
        let mut rx = agent.subscribe_events();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        let mut comp = composer.lock().await;
                        let mut s = status.lock().await;

                        match event {
                            AgentEvent::MessageUpdate {
                                thinking: t,
                                text: txt,
                                is_delta,
                                id,
                            } => {
                                if is_delta {
                                    if !t.is_empty() { comp.push_delta(id.clone(), BlockType::Thinking, &t); }
                                    if !txt.is_empty() { comp.push_delta(id, BlockType::Text, &txt); }
                                } else {
                                    if !t.is_empty() { comp.update_block_by_id(&id.clone().unwrap_or_else(|| "think".into()), BlockType::Thinking, t); }
                                    if !txt.is_empty() { comp.update_block_by_id(&id.unwrap_or_else(|| "text".into()), BlockType::Text, txt); }
                                }
                            }
                            AgentEvent::ContentSync { items } => {
                                let mapped = items.into_iter().map(|i| {
                                    match i.type_ {
                                        ContentType::Thinking => Block::new(BlockType::Thinking, i.content),
                                        ContentType::Text => Block::new(BlockType::Text, i.content),
                                        ContentType::ToolCall(n) => Block::with_label(BlockType::ToolCall, n, i.id),
                                        ContentType::ToolOutput => {
                                            let mut b = Block::new(BlockType::ToolOutput, i.content);
                                            b.id = i.id;
                                            b
                                        }
                                    }
                                }).collect();
                                comp.sync_content(mapped);
                            }
                            AgentEvent::ToolExecutionStart { id, name } => {
                                comp.set_tool_call(id, name);
                            }
                            AgentEvent::ToolExecutionUpdate { id, output } => {
                                comp.update_block_by_id(&id, BlockType::ToolOutput, output);
                            }
                            AgentEvent::AgentEnd { success, error } => {
                                *s = if success { ExecStatus::Success } else { ExecStatus::Error(error.unwrap_or_else(|| "Error".to_string())) };
                            }
                            AgentEvent::Error { message } => {
                                *s = ExecStatus::Error(message);
                            }
                            _ => {}
                        }

                        let finished = *s != ExecStatus::Running;
                        drop(comp);
                        drop(s);
                        if finished { break; }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        info!("⚠️ Writer lagged by {} messages", n);
                        continue;
                    }
                    Err(_) => break,
                }
                tokio::task::yield_now().await;
            }
        });

        let _ = render_task.await;
        typing_task.abort();
    }
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        info!("✅ Connected as {}! (ID: {})", ready.user.name, ready.user.id);
        info!("🔑 Guilds count: {}", ready.guilds.len());

        // 偵測指令註冊
        for guild in &ready.guilds {
            info!("🏰 Guild: id={}, unavailable={}", guild.id, guild.unavailable);
        }

        let commands = commands::get_all_commands().into_iter().map(|cmd| cmd.create_command()).collect::<Vec<_>>();
        match serenity::all::Command::set_global_commands(&ctx.http, commands).await {
            Ok(_) => info!("✅ Registered global commands"),
            Err(e) => error!("❌ Failed to register commands: {}", e),
        }
    }

    async fn guild_create(&self, _ctx: Context, guild: serenity::model::guild::Guild, is_new: Option<bool>) {
        info!("🏰 Guild Available: name={}, id={}, is_new={:?}", guild.name, guild.id, is_new);
        for (id, channel) in &guild.channels {
            debug!("📺 Channel: name={}, id={}", channel.name, id);
        }
    }

    async fn message(&self, ctx: Context, msg: Message) {
        if msg.author.bot { return; }
        info!("📩 Message from {}: {}", msg.author.name, msg.content);
        
        let user_id = msg.author.id.to_string();
        let channel_id_str = msg.channel_id.to_string();
        let (is_auth, mention_only) = self.state.auth.is_authorized(&user_id, &channel_id_str);

        if !is_auth {
            if msg.mentions_me(&ctx).await.unwrap_or(false) {
                if let Ok(token) = self.state.auth.create_token("channel", &channel_id_str) {
                    let _ = msg.reply(&ctx.http, format!("🔒 需要認證！\n`agent-discord auth {}`", token)).await;
                }
            }
            return;
        }
        
        if mention_only && !msg.mentions_me(&ctx).await.unwrap_or(false) { return; }

        let channel_config = ChannelConfig::load().await.unwrap_or_default();
        let agent_type = channel_config.get_agent_type(&channel_id_str);
        
        let state = self.state.clone();
        tokio::spawn(async move {
            match state.session_manager.get_or_create_session(msg.channel_id.get(), agent_type, &state.backend_manager).await {
                Ok((agent, is_new)) => {
                    Handler::start_agent_loop(agent, ctx.http.clone(), msg.channel_id, state, Some(msg.content), is_new).await;
                }
                Err(e) => error!("❌ Session error: {}", e),
            }
        });
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        if let Interaction::Command(command) = interaction {
            info!("⚔️ Command: /{}", command.data.name);
            let cmd_name = command.data.name.clone();
            
            let channel_id_str = command.channel_id.to_string();
            let channel_config = ChannelConfig::load().await.unwrap_or_default();
            let agent_type = channel_config.get_agent_type(&channel_id_str);
            
            let state = self.state.clone();
            let cmd_interaction = command.clone();
            tokio::spawn(async move {
                if let Ok((agent, _)) = state.session_manager.get_or_create_session(cmd_interaction.channel_id.get(), agent_type, &state.backend_manager).await {
                    for cmd in commands::get_all_commands() {
                        if cmd.name() == cmd_name {
                            let _ = cmd.execute(&ctx, &cmd_interaction, agent, &state).await;
                            break;
                        }
                    }
                }
            });
        } else if let Interaction::Component(component) = interaction {
            let custom_id = component.data.custom_id.as_str();
            if custom_id.starts_with("agent_") {
                let _ = handle_button(&ctx, &component, &self.state).await;
            } else if custom_id.starts_with("model_select") {
                let channel_id_str = component.channel_id.to_string();
                let agent_type = ChannelConfig::load().await.unwrap_or_default().get_agent_type(&channel_id_str);
                let state = self.state.clone();
                tokio::spawn(async move {
                    if let Ok((agent, _)) = state.session_manager.get_or_create_session(component.channel_id.get(), agent_type, &state.backend_manager).await {
                        let _ = commands::model::handle_model_select(&ctx, &component, agent, &state).await;
                    }
                });
            }
        }
    }
}

async fn run_bot() -> anyhow::Result<()> {
    migrate::run_migrations().await?;
    let config = Arc::new(Config::load().await?);
    let state = AppState {
        config: config.clone(),
        session_manager: Arc::new(SessionManager::new(config.clone())),
        auth: Arc::new(AuthManager::new()),
        i18n: Arc::new(RwLock::new(I18n::new(&config.language))),
        backend_manager: Arc::new(agent::manager::BackendManager::new(config.clone())),
    };
    let mut client = Client::builder(&state.config.discord_token, GatewayIntents::GUILD_MESSAGES | GatewayIntents::MESSAGE_CONTENT | GatewayIntents::GUILDS | GatewayIntents::DIRECT_MESSAGES)
        .event_handler(Handler { state }).await?;
    client.start().await?;
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();
    let cli = Cli::parse();
    match cli.command {
        Some(Commands::Run) => run_bot().await?,
        Some(Commands::Version) => println!("v{}", env!("CARGO_PKG_VERSION")),
        _ => { /* manage daemon logic skipped for brevity */ run_bot().await? }
    }
    Ok(())
}
