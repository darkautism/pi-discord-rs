use agent::{AgentEvent, AiAgent, ContentType, UserInput};
use clap::{Parser, Subcommand};
use rust_embed::RustEmbed;
use serenity::all::{
    Context, CreateEmbed, CreateInteractionResponse, CreateInteractionResponseMessage,
    CreateMessage, EditMessage, EventHandler, GatewayIntents, Interaction, Message, Ready,
};
use serenity::async_trait;
use serenity::Client;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, Level};

mod cron;
mod i18n;

mod agent;
mod auth;
mod commands;
mod composer;
mod config;
mod migrate;
mod session;
mod uploads;

use auth::AuthManager;
use commands::agent::{handle_button, ChannelConfig};
use composer::{Block, BlockType, EmbedComposer};
use config::Config;
use cron::CronManager;
use i18n::I18n;
use session::SessionManager;
use uploads::UploadManager;

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
#[folder = "prompts/"]
struct DefaultPrompts;

type ActiveRenderMap = HashMap<u64, (serenity::model::id::MessageId, Vec<JoinHandle<()>>)>;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub session_manager: Arc<SessionManager>,
    pub auth: Arc<AuthManager>,
    pub i18n: Arc<RwLock<I18n>>,
    pub backend_manager: Arc<agent::manager::BackendManager>,
    pub cron_manager: Arc<CronManager>,
    pub active_renders: Arc<Mutex<ActiveRenderMap>>,
    pub upload_manager: Arc<UploadManager>,
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

pub struct Handler {
    state: AppState,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExecStatus {
    Running,
    Success,
    Error(String),
}

impl Handler {
    pub async fn start_agent_loop(
        agent: Arc<dyn AiAgent>,
        http: Arc<serenity::http::Http>,
        channel_id: serenity::model::id::ChannelId,
        state: AppState,
        initial_input: Option<UserInput>,
        is_brand_new: bool,
    ) {
        let channel_id_u64 = channel_id.get();

        // 1. [搶佔邏輯]: 如果該頻道有正在運行的任務，立即中斷並刪除該訊息
        {
            let mut active = state.active_renders.lock().await;
            if let Some((old_msg_id, handles)) = active.remove(&channel_id_u64) {
                for h in handles {
                    h.abort();
                }
                let http_del = http.clone();
                tokio::spawn(async move {
                    if let Err(e) = channel_id.delete_message(&http_del, old_msg_id).await {
                        error!("❌ Failed to delete preempted message: {}", e);
                    }
                });
                info!(
                    "🗑️ Preempted unfinished response in channel {}",
                    channel_id_u64
                );
            }
        }

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
        let assistant_name = {
            let channel_cfg = ChannelConfig::load().await.unwrap_or_default();
            channel_cfg
                .channels
                .get(&channel_id.to_string())
                .and_then(|e| e.assistant_name.clone())
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| state.config.assistant_name.clone())
        };

        // --- 任務啟動：收集所有 Handles ---
        let mut handles = Vec::new();

        if let Some(mut input) = initial_input {
            let mut final_msg = input.text;
            if is_brand_new {
                let prompts = load_all_prompts();
                if !prompts.is_empty() {
                    final_msg = format!("{}\n\n{}", prompts, final_msg);
                }
            }
            input.text = final_msg;
            let agent_for_prompt = Arc::clone(&agent);
            let status_for_prompt = Arc::clone(&status);
            let composer_for_prompt = Arc::clone(&composer);
            handles.push(tokio::spawn(async move {
                if let Err(e) = agent_for_prompt.prompt_with_input(&input).await {
                    let mut s = status_for_prompt.lock().await;
                    let comp = composer_for_prompt.lock().await;
                    if *s == ExecStatus::Running {
                        if comp.blocks.is_empty() {
                            *s = ExecStatus::Error(e.to_string());
                        } else {
                            info!("⚠️ POST prompt reported error: {}, but SSE stream is active. Continuing...", e);
                        }
                    }
                }
            }));
        }

        let typing_http = http.clone();
        let typing_status = Arc::clone(&status);
        handles.push(tokio::spawn(async move {
            loop {
                {
                    let s = typing_status.lock().await;
                    if *s != ExecStatus::Running {
                        break;
                    }
                }
                let _ = channel_id.broadcast_typing(&typing_http).await;
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }));

        // --- 任務 A: Render 循環 ---
        let render_status = Arc::clone(&status);
        let render_composer = Arc::clone(&composer);
        let render_http = http.clone();
        let mut render_msg = discord_msg.clone();
        let render_i18n = Arc::clone(&state.i18n);
        let render_state = state.clone();
        let render_assistant_name = assistant_name.clone();
        let render_channel_id = channel_id;
        let render_msg_id = discord_msg.id;

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
                                .description(format!(
                                    "{}\n\n{} {}",
                                    desc,
                                    i18n.get("runtime_error_prefix"),
                                    e
                                ));
                        }
                        ExecStatus::Success => {
                            let title = i18n
                                .get_args("agent_response", &[render_assistant_name.clone()]);
                            embed = embed
                                .title(title)
                                .color(0x00ff00)
                                .description(if desc.is_empty() {
                                    i18n.get("done")
                                } else {
                                    desc.clone()
                                });
                        }
                        ExecStatus::Running => {
                            let title = i18n
                                .get_args("agent_working", &[render_assistant_name.clone()]);
                            embed = embed
                                .title(title)
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
                    // 完工：從活躍任務中移除自己
                    let mut active = render_state.active_renders.lock().await;
                    if let Some((active_msg_id, _)) = active.get(&channel_id_u64) {
                        if *active_msg_id == render_msg_id {
                            active.remove(&channel_id_u64);
                            info!(
                                "✅ Completed response registered as historical for channel {}",
                                channel_id_u64
                            );
                        }
                    }
                    break;
                }
            }
        });

        // --- 任務 B: Writer 任務 ---
        let mut rx = agent.subscribe_events();
        let writer_status = Arc::clone(&status);
        let writer_composer = Arc::clone(&composer);
        let writer_task = tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        let mut comp = writer_composer.lock().await;
                        let mut s = writer_status.lock().await;

                        match event {
                            AgentEvent::MessageUpdate {
                                thinking: t,
                                text: txt,
                                is_delta,
                                id,
                            } => {
                                if is_delta {
                                    if !t.is_empty() {
                                        comp.push_delta(id.clone(), BlockType::Thinking, &t);
                                    }
                                    if !txt.is_empty() {
                                        comp.push_delta(id, BlockType::Text, &txt);
                                    }
                                } else {
                                    if !t.is_empty() {
                                        comp.update_block_by_id(
                                            &id.clone().unwrap_or_else(|| "think".into()),
                                            BlockType::Thinking,
                                            t,
                                        );
                                    }
                                    if !txt.is_empty() {
                                        comp.update_block_by_id(
                                            &id.unwrap_or_else(|| "text".into()),
                                            BlockType::Text,
                                            txt,
                                        );
                                    }
                                }
                            }
                            AgentEvent::ContentSync { items } => {
                                let mapped = items
                                    .into_iter()
                                    .map(|i| match i.type_ {
                                        ContentType::Thinking => {
                                            Block::new(BlockType::Thinking, i.content)
                                        }
                                        ContentType::Text => Block::new(BlockType::Text, i.content),
                                        ContentType::ToolCall(n) => {
                                            Block::with_label(BlockType::ToolCall, n, i.id)
                                        }
                                        ContentType::ToolOutput => {
                                            let mut b =
                                                Block::new(BlockType::ToolOutput, i.content);
                                            b.id = i.id;
                                            b
                                        }
                                    })
                                    .collect();
                                comp.sync_content(mapped);
                            }
                            AgentEvent::ToolExecutionStart { id, name } => {
                                comp.set_tool_call(id, name);
                            }
                            AgentEvent::ToolExecutionUpdate { id, output } => {
                                comp.update_block_by_id(&id, BlockType::ToolOutput, output);
                            }
                            AgentEvent::AgentEnd { success, error } => {
                                *s = if success {
                                    ExecStatus::Success
                                } else {
                                    ExecStatus::Error(error.unwrap_or_else(|| "Error".to_string()))
                                };
                            }
                            AgentEvent::Error { message } => {
                                *s = ExecStatus::Error(message);
                            }
                            _ => {}
                        }

                        let finished = *s != ExecStatus::Running;
                        drop(comp);
                        drop(s);
                        if finished {
                            break;
                        }
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

        // 登記新任務
        handles.push(render_task);
        handles.push(writer_task);
        {
            let mut active = state.active_renders.lock().await;
            active.insert(channel_id_u64, (discord_msg.id, handles));
        }
    }
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        info!(
            "✅ Connected as {}! (ID: {})",
            ready.user.name, ready.user.id
        );
        info!("🔑 Guilds count: {}", ready.guilds.len());

        // 偵測指令註冊
        for guild in &ready.guilds {
            info!(
                "🏰 Guild: id={}, unavailable={}",
                guild.id, guild.unavailable
            );
        }

        let i18n = self.state.i18n.read().await;
        let commands = commands::get_all_commands()
            .into_iter()
            .map(|cmd| cmd.create_command(&i18n))
            .collect::<Vec<_>>();
        drop(i18n);

        match serenity::all::Command::set_global_commands(&ctx.http, commands).await {
            Ok(_) => info!("✅ Registered global commands"),
            Err(e) => error!("❌ Failed to register commands: {}", e),
        }
    }

    async fn guild_create(
        &self,
        _ctx: Context,
        guild: serenity::model::guild::Guild,
        is_new: Option<bool>,
    ) {
        info!(
            "🏰 Guild Available: name={}, id={}, is_new={:?}",
            guild.name, guild.id, is_new
        );
        for (id, channel) in &guild.channels {
            debug!("📺 Channel: name={}, id={}", channel.name, id);
        }
    }

    async fn message(&self, ctx: Context, msg: Message) {
        if msg.author.bot {
            return;
        }

        // 1. 過濾討論串建立等系統訊息 (僅處理 Regular 訊息)
        if msg.kind != serenity::all::MessageType::Regular
            && msg.kind != serenity::all::MessageType::InlineReply
        {
            return;
        }

        info!("📩 Message from {}: {}", msg.author.name, msg.content);

        let user_id = msg.author.id.to_string();
        let (is_auth, mention_only) = self
            .state
            .auth
            .is_authorized_with_thread(&ctx, &user_id, msg.channel_id)
            .await;

        let channel_id_str = msg.channel_id.to_string();

        if !is_auth {
            if msg.mentions_me(&ctx).await.unwrap_or(false) {
                if let Ok(token) = self.state.auth.create_token("channel", &channel_id_str) {
                    let auth_msg = {
                        let i18n = self.state.i18n.read().await;
                        i18n.get_args("auth_required_cmd", &[token])
                    };
                    let _ = msg
                        .reply(&ctx.http, auth_msg)
                        .await;
                }
            }
            return;
        }

        if mention_only && !msg.mentions_me(&ctx).await.unwrap_or(false) {
            return;
        }

        let channel_config = ChannelConfig::load().await.unwrap_or_default();
        let agent_type = channel_config.get_agent_type(&channel_id_str);
        let files = self
            .state
            .upload_manager
            .stage_attachments(msg.channel_id.get(), &msg.attachments)
            .await;
        let input = UserInput {
            text: msg.content.clone(),
            files,
        };

        let state = self.state.clone();
        tokio::spawn(async move {
            match state
                .session_manager
                .get_or_create_session(msg.channel_id.get(), agent_type, &state.backend_manager)
                .await
            {
                Ok((agent, is_new)) => {
                    Handler::start_agent_loop(
                        agent,
                        ctx.http.clone(),
                        msg.channel_id,
                        state,
                        Some(input),
                        is_new,
                    )
                    .await;
                }
                Err(e) => {
                    error!("❌ Session error: {}", e);
                    let err_text = e.to_string();
                    let channel_config = ChannelConfig::load().await.unwrap_or_default();
                    let backend = channel_config.get_agent_type(&msg.channel_id.to_string());
                    let user_msg = {
                        let i18n = state.i18n.read().await;
                        crate::commands::agent::build_backend_error_message(
                            &i18n,
                            backend,
                            &err_text,
                            state.config.opencode.port,
                        )
                    };
                    let _ = msg.reply(&ctx.http, user_msg).await;
                }
            }
        });
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        if let Interaction::Command(command) = interaction {
            info!("⚔️ Command: /{}", command.data.name);

            let user_id = command.user.id.to_string();
            let (is_auth, _) = self
                .state
                .auth
                .is_authorized_with_thread(&ctx, &user_id, command.channel_id)
                .await;

            if !is_auth {
                let not_auth_msg = {
                    let i18n = self.state.i18n.read().await;
                    i18n.get("mention_not_auth")
                };
                let _ = command
                    .create_response(
                        &ctx.http,
                        CreateInteractionResponse::Message(
                            CreateInteractionResponseMessage::new()
                                .content(not_auth_msg)
                                .ephemeral(true),
                        ),
                    )
                    .await;
                return;
            }

            let cmd_name = command.data.name.clone();
            let state = self.state.clone();
            let cmd_interaction = command.clone();
            tokio::spawn(async move {
                for cmd in commands::get_all_commands() {
                    if cmd.name() == cmd_name {
                        let _ = cmd.execute(&ctx, &cmd_interaction, &state).await;
                        break;
                    }
                }
            });
        } else if let Interaction::Modal(modal) = interaction {
            let custom_id = modal.data.custom_id.as_str();
            if custom_id == "cron_setup" {
                let state = self.state.clone();
                tokio::spawn(async move {
                    let _ = commands::cron::handle_modal_submit(&ctx, &modal, &state).await;
                });
            } else if custom_id == "config_assistant_modal" {
                let state = self.state.clone();
                tokio::spawn(async move {
                    let _ = commands::config::handle_assistant_modal_submit(&ctx, &modal, &state)
                        .await;
                });
            }
        } else if let Interaction::Component(component) = interaction {
            let custom_id = component.data.custom_id.as_str();
            if custom_id.starts_with("config_") {
                let _ = commands::config::handle_config_select(&ctx, &component, &self.state).await;
            } else if custom_id.starts_with("agent_") {
                let _ = handle_button(&ctx, &component, &self.state).await;
            } else if custom_id == "cron_delete_select" {
                let state = self.state.clone();
                tokio::spawn(async move {
                    let _ = commands::cron::handle_delete_select(&ctx, &component, &state).await;
                });
            } else if custom_id.starts_with("model_select") {
                let state = self.state.clone();
                tokio::spawn(async move {
                    let channel_id_str = component.channel_id.to_string();
                    let channel_config = ChannelConfig::load().await.unwrap_or_default();
                    let agent_type = channel_config.get_agent_type(&channel_id_str);

                    if let Ok((agent, _)) = state
                        .session_manager
                        .get_or_create_session(
                            component.channel_id.get(),
                            agent_type,
                            &state.backend_manager,
                        )
                        .await
                    {
                        let _ =
                            commands::model::handle_model_select(&ctx, &component, agent, &state)
                                .await;
                    }
                });
            }
        }
    }
}

async fn run_bot() -> anyhow::Result<()> {
    migrate::run_migrations().await?;
    let config = Arc::new(Config::load().await?);
    let cron_manager = Arc::new(CronManager::new().await?);
    if let Err(e) = cron_manager.load_from_disk().await {
        error!("❌ Failed to load cron jobs from disk: {}", e);
    }
    let state = Arc::new(AppState {
        config: config.clone(),
        session_manager: Arc::new(SessionManager::new(config.clone())),
        auth: Arc::new(AuthManager::new()),
        i18n: Arc::new(RwLock::new(I18n::new(&config.language))),
        backend_manager: Arc::new(agent::manager::BackendManager::new(config.clone())),
        cron_manager,
        active_renders: Arc::new(Mutex::new(HashMap::new())),
        upload_manager: Arc::new(UploadManager::new(
            20 * 1024 * 1024,
            std::time::Duration::from_secs(24 * 60 * 60),
            std::time::Duration::from_secs(10 * 60),
        )?),
    });
    let mut client = Client::builder(
        &state.config.discord_token,
        GatewayIntents::GUILD_MESSAGES
            | GatewayIntents::MESSAGE_CONTENT
            | GatewayIntents::GUILDS
            | GatewayIntents::DIRECT_MESSAGES,
    )
    .event_handler(Handler {
        state: (*state).clone(),
    })
    .await?;

    // 初始化 CronManager 的執行環境
    state
        .cron_manager
        .init(client.http.clone(), Arc::downgrade(&state))
        .await;

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
        Some(Commands::Daemon { action }) => {
            let service_path = dirs::config_dir()
                .or_else(dirs::home_dir)
                .ok_or_else(|| anyhow::anyhow!("Cannot determine config/home directory"))?
                .join("systemd")
                .join("user")
                .join("agent-discord-rs.service");

            match action {
                DaemonAction::Enable => {
                    // 1. 偵測目前執行檔路徑
                    let exe_path = std::env::current_exe()?.to_string_lossy().to_string();

                    // 2. 偵測時區
                    let tz = std::fs::read_to_string("/etc/timezone")
                        .unwrap_or_else(|_| "UTC".to_string())
                        .trim()
                        .to_string();

                    // 3. 取得目前環境變數
                    let current_path = std::env::var("PATH").unwrap_or_default();
                    let augmented_path =
                        agent::manager::BackendManager::build_augmented_path(&current_path);

                    let service_content = format!(
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
                    );

                    std::fs::create_dir_all(service_path.parent().unwrap())?;
                    std::fs::write(&service_path, service_content)?;

                    // 4. 啟動服務
                    let _ = std::process::Command::new("systemctl")
                        .args(["--user", "daemon-reload"])
                        .status();
                    let _ = std::process::Command::new("systemctl")
                        .args(["--user", "enable", "--now", "agent-discord-rs.service"])
                        .status();

                    println!(
                        "✅ Daemon enabled and started at {}",
                        service_path.display()
                    );
                    println!("   Exe: {}", exe_path);
                    println!("   TZ:  {}", tz);
                }
                DaemonAction::Disable => {
                    let _ = std::process::Command::new("systemctl")
                        .args(["--user", "disable", "--now", "agent-discord-rs.service"])
                        .status();
                    if service_path.exists() {
                        std::fs::remove_file(service_path)?;
                    }
                    println!("🛑 Daemon disabled and service file removed.");
                }
            }
        }
        _ => run_bot().await?,
    }
    Ok(())
}
