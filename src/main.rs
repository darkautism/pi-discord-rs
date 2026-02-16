use agent::{AgentEvent, AiAgent, ContentType, NoOpAgent};
use clap::{CommandFactory, Parser, Subcommand};
use rust_embed::RustEmbed;
use serenity::all::{
    CommandInteraction, Context, CreateEmbed, CreateMessage, EditMessage, EventHandler,
    GatewayIntents, Interaction, Message, Ready,
};
use serenity::async_trait;
use serenity::Client;
use std::process::Command as StdCommand;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info, Level};

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
        let mut rx = agent.subscribe_events();
        let i18n = state.i18n.read().await;
        let processing_msg = i18n.get("processing");
        drop(i18n);

        let mut discord_msg = match channel_id
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

        let mut composer = EmbedComposer::new(3900);
        let mut status = ExecStatus::Running;

        if let Some(msg) = initial_message {
            let mut final_msg = msg;
            if is_brand_new {
                let prompts = load_all_prompts();
                if !prompts.is_empty() {
                    final_msg = format!("{}\n\n{}", prompts, final_msg);
                }
            }
            if let Err(e) = agent.prompt(&final_msg).await {
                status = ExecStatus::Error(e.to_string());
            }
        }

        if status == ExecStatus::Running {
            let mut last_upd = std::time::Instant::now();

            let typing_http = http.clone();
            let typing_task = tokio::spawn(async move {
                loop {
                    let _ = channel_id.broadcast_typing(&typing_http).await;
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            });

            while let Ok(event) = rx.recv().await {
                match event {
                    AgentEvent::MessageUpdate {
                        thinking: t,
                        text: txt,
                        is_delta,
                        id,
                    } => {
                        if is_delta {
                            if !t.is_empty() {
                                composer.push_delta(BlockType::Thinking, &t);
                            }
                            if !txt.is_empty() {
                                composer.push_delta(BlockType::Text, &txt);
                            }
                        } else {
                            // 如果有 ID，精準更新；否則使用默認 ID
                            let think_id = id.as_deref().unwrap_or("sync-thinking");
                            let text_id = id.as_deref().unwrap_or("sync-text");

                            if !t.is_empty() {
                                composer.update_block_by_id(think_id, BlockType::Thinking, t);
                            }
                            if !txt.is_empty() {
                                composer.update_block_by_id(text_id, BlockType::Text, txt);
                            }
                        }
                    }
                    AgentEvent::ContentSync { items } => {
                        let mapped = items
                            .into_iter()
                            .map(|i| {
                                match i.type_ {
                                    ContentType::Thinking => {
                                        Block::new(BlockType::Thinking, i.content)
                                    }
                                    ContentType::Text => Block::new(BlockType::Text, i.content),
                                    ContentType::ToolCall(n) => {
                                        // 如果名稱已經包含 Emoji，就不再重複添加
                                        let label = if n.contains("🛠️") {
                                            n.to_string()
                                        } else {
                                            format!("🛠️ `{}`", n)
                                        };
                                        Block::with_label(BlockType::ToolCall, label, i.id)
                                    }
                                    ContentType::ToolOutput => {
                                        let mut b = Block::new(BlockType::ToolOutput, i.content);
                                        b.id = i.id;
                                        b
                                    }
                                }
                            })
                            .collect();
                        composer.sync_content(mapped);
                    }
                    AgentEvent::ToolExecutionStart { id, name } => {
                        let label = format!("🛠️ `{}`", name);
                        composer.set_tool_call(id, label);
                    }
                    AgentEvent::ToolExecutionUpdate { id, output } => {
                        composer.update_block_by_id(&id, BlockType::ToolOutput, output);
                    }
                    AgentEvent::AutoRetry { attempt, max } => {
                        composer.push_delta(
                            BlockType::Status,
                            &format!("🔄 **自動重試 ({}/{})** - API 暫時限制中...", attempt, max),
                        );
                        last_upd = std::time::Instant::now() - std::time::Duration::from_secs(10);
                    }
                    AgentEvent::AgentEnd { success, error } => {
                        status = if success {
                            ExecStatus::Success
                        } else {
                            ExecStatus::Error(error.unwrap_or_else(|| "Error".to_string()))
                        };
                    }
                    AgentEvent::Error { message } => {
                        // 這裡如果是 Kilo 背景錯誤已經被過濾了
                        // 其他嚴重錯誤則轉化為狀態，不直接推送到 Status 塊以免重複
                        status = ExecStatus::Error(message);
                    }
                    _ => {}
                }

                if last_upd.elapsed() >= std::time::Duration::from_millis(1500)
                    || status != ExecStatus::Running
                {
                    let mut embed = CreateEmbed::new();
                    let i18n = state.i18n.read().await;
                    let desc = composer.render();

                    info!("📢 [FINAL-EMBED-{}]:\n{}\n---", channel_id, desc);

                    match &status {
                        ExecStatus::Error(e) => {
                            // 如果有渲染內容，保留內容並在下方附帶錯誤
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
                                    desc
                                });
                        }
                        ExecStatus::Running => {
                            embed = embed
                                .title(i18n.get("pi_working"))
                                .color(0xFFA500)
                                .description(if desc.is_empty() {
                                    i18n.get("wait")
                                } else {
                                    desc
                                });
                        }
                    }
                    let _ = discord_msg
                        .edit(&http, EditMessage::new().embed(embed))
                        .await;
                    last_upd = std::time::Instant::now();
                    if status != ExecStatus::Running {
                        typing_task.abort();
                        break;
                    }
                }
            }
            typing_task.abort();
        } else {
            // 如果 status 在進入 loop 前就已經是 Error
            let mut embed = CreateEmbed::new();
            let i18n = state.i18n.read().await;
            if let ExecStatus::Error(e) = status {
                embed = embed
                    .title(i18n.get("api_error"))
                    .color(0xff0000)
                    .description(format!("❌ **錯誤:** {}", e));
                let _ = discord_msg
                    .edit(&http, EditMessage::new().embed(embed))
                    .await;
            }
        }
    }
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        info!("✅ Connected as {}!", ready.user.name);
        info!("🔑 Guilds count: {}", ready.guilds.len());

        // 偵測指令註冊
        let commands = commands::get_all_commands()
            .into_iter()
            .map(|cmd| cmd.create_command())
            .collect::<Vec<_>>();
        let cmd_count = commands.len();
        match serenity::all::Command::set_global_commands(&ctx.http, commands).await {
            Ok(_) => info!("✅ Successfully registered {} global commands", cmd_count),
            Err(e) => error!("❌ Failed to register commands: {}", e),
        }
    }

    async fn message(&self, ctx: Context, msg: Message) {
        if msg.author.bot {
            return;
        }
        info!(
            "📩 Received message from {}: {}",
            msg.author.name, msg.content
        );
        let user_id = msg.author.id.to_string();
        let channel_id_str = msg.channel_id.to_string();

        let (is_auth, mention_only) = self.state.auth.is_authorized(&user_id, &channel_id_str);
        info!(
            "🔐 Auth status: is_auth={}, mention_only={}",
            is_auth, mention_only
        );

        if !is_auth {
            let is_dm = msg.guild_id.is_none();
            if is_dm || msg.mentions_me(&ctx).await.unwrap_or(false) {
                info!("🔒 Generating auth token for unauthorized user/channel");
                if let Ok(token) = self.state.auth.create_token(
                    if is_dm { "user" } else { "channel" },
                    if is_dm { &user_id } else { &channel_id_str },
                ) {
                    let _ = msg
                        .reply(
                            &ctx.http,
                            format!("🔒 需要認證！\n`agent-discord auth {}`", token),
                        )
                        .await;
                }
            }
            return;
        }
        if mention_only && !msg.mentions_me(&ctx).await.unwrap_or(false) && msg.guild_id.is_some() {
            info!("🔇 Message ignored: mention_only is active but no mention found");
            return;
        }

        let channel_config = ChannelConfig::load().await.unwrap_or_default();
        let agent_type = channel_config.get_agent_type(&channel_id_str);
        info!("🤖 Target agent type: {}", agent_type);

        match self
            .state
            .session_manager
            .get_or_create_session(msg.channel_id.get(), agent_type)
            .await
        {
            Ok((agent, is_brand_new)) => {
                info!("✅ Session obtained (new={})", is_brand_new);
                let content = if msg.content.starts_with('!') {
                    &msg.content[1..]
                } else {
                    &msg.content
                }
                .to_string();
                let handler_state = self.state.clone();
                tokio::spawn(async move {
                    info!("🚀 Starting agent loop thread");
                    Handler::start_agent_loop(
                        agent,
                        ctx.http.clone(),
                        msg.channel_id,
                        handler_state,
                        Some(content),
                        is_brand_new,
                    )
                    .await;
                });
            }
            Err(e) => {
                error!("❌ Failed to get/create session: {}", e);
                let _ = msg
                    .reply(&ctx.http, format!("❌ 無法初始化會話: {}", e))
                    .await;
            }
        }
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        match interaction {
            Interaction::Command(command) => self.handle_slash_command(&ctx, &command).await,
            Interaction::Component(component) => {
                let custom_id = component.data.custom_id.as_str();
                if custom_id.starts_with("agent_") {
                    let _ = handle_button(&ctx, &component, &self.state).await;
                } else if custom_id.starts_with("model_select") {
                    let channel_id_str = component.channel_id.to_string();
                    let agent_type = ChannelConfig::load()
                        .await
                        .unwrap_or_default()
                        .get_agent_type(&channel_id_str);
                    if let Ok((agent, _)) = self
                        .state
                        .session_manager
                        .get_or_create_session(component.channel_id.get(), agent_type)
                        .await
                    {
                        let _ = commands::model::handle_model_select(&ctx, &component, agent).await;
                    }
                }
            }
            _ => {}
        }
    }
}

impl Handler {
    async fn handle_slash_command(&self, ctx: &Context, command: &CommandInteraction) {
        let cmd_name = command.data.name.as_str();
        if cmd_name == "agent" {
            for cmd in commands::get_all_commands() {
                if cmd.name() == cmd_name {
                    let _ = cmd.execute(ctx, command, Arc::new(NoOpAgent)).await;
                    return;
                }
            }
        }
        let channel_id_str = command.channel_id.to_string();
        let agent_type = ChannelConfig::load()
            .await
            .unwrap_or_default()
            .get_agent_type(&channel_id_str);
        if let Ok((agent, _)) = self
            .state
            .session_manager
            .get_or_create_session(command.channel_id.get(), agent_type)
            .await
        {
            for cmd in commands::get_all_commands() {
                if cmd.name() == cmd_name {
                    let _ = cmd.execute(ctx, command, agent).await;
                    return;
                }
            }
        }
    }
}

async fn run_bot() -> anyhow::Result<()> {
    migrate::run_migrations().await?;
    let config = Arc::new(Config::load().await?);
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();
    let state = AppState {
        config: config.clone(),
        session_manager: Arc::new(SessionManager::new(config.clone())),
        auth: Arc::new(AuthManager::new()),
        i18n: Arc::new(RwLock::new(I18n::new(&config.language))),
    };
    let token = state.config.discord_token.clone();
    let mut client = Client::builder(
        &token,
        GatewayIntents::GUILD_MESSAGES
            | GatewayIntents::MESSAGE_CONTENT
            | GatewayIntents::GUILDS
            | GatewayIntents::DIRECT_MESSAGES,
    )
    .event_handler(Handler { state })
    .await?;
    client.start().await?;
    Ok(())
}

fn manage_daemon(action: DaemonAction) -> anyhow::Result<()> {
    let service_name = "agent-discord-rs";
    let base_dir = migrate::get_base_dir();
    let service_file = base_dir
        .join("systemd")
        .join(format!("{}.service", service_name));
    match action {
        DaemonAction::Enable => {
            std::fs::create_dir_all(base_dir.join("systemd"))?;
            let exe = std::env::current_exe()?;
            std::fs::write(&service_file, format!("[Unit]\nDescription=Agent Discord RS\nAfter=network.target\n\n[Service]\nType=simple\nExecStart={} run\nRestart=on-failure\nRestartSec=5s\n\n[Install]\nWantedBy=default.target\n", exe.display()))?;
            let link = dirs::home_dir()
                .unwrap()
                .join(".config/systemd/user")
                .join(format!("{}.service", service_name));
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(&service_file, &link)?;
            let _ = StdCommand::new("systemctl")
                .arg("--user")
                .arg("daemon-reload")
                .status();
            let _ = StdCommand::new("systemctl")
                .arg("--user")
                .arg("enable")
                .arg(service_name)
                .status();
            let _ = StdCommand::new("systemctl")
                .arg("--user")
                .arg("start")
                .arg(service_name)
                .status();
        }
        DaemonAction::Disable => {
            let _ = StdCommand::new("systemctl")
                .arg("--user")
                .arg("stop")
                .arg(service_name)
                .status();
            let _ = StdCommand::new("systemctl")
                .arg("--user")
                .arg("disable")
                .arg(service_name)
                .status();
            let _ = std::fs::remove_file(
                dirs::home_dir()
                    .unwrap()
                    .join(".config/systemd/user")
                    .join(format!("{}.service", service_name)),
            );
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
            let _ = StdCommand::new("systemctl")
                .arg("--user")
                .arg("kill")
                .arg("-s")
                .arg("HUP")
                .arg("agent-discord-rs")
                .status();
            println!("✅ Reload sent.");
        }
        Some(Commands::Auth { token }) => {
            let manager = AuthManager::new();
            if let Ok((t, id)) = manager.redeem_token(&token) {
                println!("✅ Authorized {} {}.", t, id);
            }
        }
        Some(Commands::Version) => println!("v{}", env!("CARGO_PKG_VERSION")),
        None => Cli::command().print_help()?,
    }
    Ok(())
}
