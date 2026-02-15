use agent::{AgentEvent, AiAgent, NoOpAgent};
use clap::{Parser, Subcommand};
use rust_embed::RustEmbed;
use serenity::all::{
    CommandInteraction, Context, CreateEmbed, CreateInteractionResponse,
    CreateInteractionResponseMessage, CreateMessage, EditMessage, EventHandler, GatewayIntents,
    Interaction, Message, Ready,
};
use serenity::async_trait;
use serenity::Client;
use std::process::Command as StdCommand;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info, warn, Level};

mod agent;
mod auth;
mod commands;
mod composer;
mod config;
mod migrate;
mod session;

use auth::AuthManager;
use commands::agent::{handle_button, ChannelConfig};
use composer::{BlockType, EmbedComposer};
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
                .expect("Invalid UTF-8 in locale")
                .to_string()
        } else {
            warn!("Locale {} not found, defaulting to zh-TW", lang);
            r#"{"processing": "處理中...", "api_error": "API 錯誤", "user_aborted": "已中止", "aborted_desc": "使用者已中止操作。", "pi_response": "AI 回應", "pi_working": "思考中...", "wait": "請稍候...", "abort_sent": "已發送中止信號。", "loading_skill": "正在載入 skill {}...", "exec_success": "✅ {}", "exec_failed": "❌ {}"}"#.to_string()
        };
        let texts = serde_json::from_str(&content).expect("Failed to parse locale");
        I18n { texts }
    }

    fn get(&self, key: &str) -> String {
        self.texts
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or(key)
            .to_string()
    }

    fn get_arg(&self, key: &str, arg: &str) -> String {
        self.get(key).replace("{}", arg)
    }
}

fn load_all_prompts() -> String {
    let prompts_dir = migrate::get_prompts_dir();
    let _ = std::fs::create_dir_all(&prompts_dir);

    // 1. Get files from disk
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&prompts_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
                if ext == "txt" || ext == "md" {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        files.push((path.file_name().unwrap().to_owned(), content));
                    }
                }
            }
        }
    }

    // 2. If no files, try to load from embed
    if files.is_empty() {
        for file in DefaultPrompts::iter() {
            if let Some(content) = DefaultPrompts::get(&file) {
                if let Ok(s) = std::str::from_utf8(content.data.as_ref()) {
                    let path = prompts_dir.join(file.as_ref());
                    let _ = std::fs::write(&path, s);
                    files.push((file.as_ref().into(), s.to_string()));
                }
            }
        }
    }

    // 3. Sort by filename and concatenate
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files.into_iter().map(|(_, content)| content).collect::<Vec<_>>().join("\n\n")
}

struct Handler {
    state: AppState,
}

#[derive(Clone, Debug, PartialEq)]
enum ExecStatus {
    Running,
    Success,
    Error(String),
    Aborted,
}

impl Handler {
    fn safe_truncate(s: &str, max: usize) -> String {
        let count = s.chars().count();
        if count > max {
            let skip = count - max;
            format!("...{}", s.chars().skip(skip).collect::<String>())
        } else {
            s.to_string()
        }
    }

    async fn start_agent_loop(
        agent: Arc<dyn AiAgent>,
        http: Arc<serenity::http::Http>,
        channel_id: serenity::model::id::ChannelId,
        state: AppState,
        initial_message: Option<String>,
    ) {
        let mut rx = agent.subscribe_events();

        // 發送初始訊息
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
                error!("Failed to send initial message: {}", e);
                return;
            }
        };

        if let Some(msg) = initial_message {
            let mut final_msg = msg;
            
            // 如果是新會話，加入系統提示詞
            if let Ok(state) = agent.get_state().await {
                if state.message_count == 0 {
                    let prompts = load_all_prompts();
                    if !prompts.is_empty() {
                        info!("📝 Prepending system prompts to the first message");
                        final_msg = format!("{}\n\n{}", prompts, final_msg);
                    }
                }
            }

            if let Err(e) = agent.prompt(&final_msg).await {
                error!("Failed to send initial prompt: {}", e);
                let _ = discord_msg
                    .edit(&http, EditMessage::new().content(format!("❌ Error: {}", e)))
                    .await;
                return;
            }
        }

        let mut composer = EmbedComposer::new(3800); // 預留空間給標題與錯誤訊息
        let mut status = ExecStatus::Running;
        let mut last_upd = std::time::Instant::now();

        // Typing indicator task
        let typing_http = http.clone();
        let typing_channel = channel_id;
        let typing_task = tokio::spawn(async move {
            loop {
                let _ = typing_channel.broadcast_typing(&typing_http).await;
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        });

        while let Ok(event) = rx.recv().await {
            match event {
                AgentEvent::MessageUpdate { thinking: t, text: txt, is_delta } => {
                    if is_delta {
                        if !t.is_empty() { composer.push_delta(BlockType::Thinking, &t); }
                        if !txt.is_empty() { composer.push_delta(BlockType::Text, &txt); }
                    } else {
                        // 非 Delta 模式採用更新模式，防止重複區塊堆疊
                        if !t.is_empty() { composer.update_last_block(BlockType::Thinking, t); }
                        if !txt.is_empty() { composer.update_last_block(BlockType::Text, txt); }
                    }
                }
                AgentEvent::ToolExecutionStart { name } => {
                    composer.update_last_block(BlockType::Text, format!("🛠️ **正在執行工具:** `{}`", name));
                }
                AgentEvent::ToolExecutionUpdate { output } => {
                    let char_vec: Vec<char> = output.chars().collect();
                    let truncated = if char_vec.len() > 200 {
                        format!("...{}", char_vec[char_vec.len() - 200..].iter().collect::<String>())
                    } else {
                        output
                    };
                    composer.update_last_block(BlockType::Tool, truncated);
                }
                AgentEvent::ToolExecutionEnd { .. } => {
                    // 工具結束後可選擇是否保留精簡日誌
                }
                AgentEvent::AutoRetry { attempt, max } => {
                    composer.update_last_block(BlockType::Text, format!("🔄 **自動重試** ({}/{})", attempt, max));
                }
                AgentEvent::AgentEnd { success, error } => {
                    status = if success {
                        ExecStatus::Success
                    } else {
                        ExecStatus::Error(error.unwrap_or_else(|| "Unknown error".to_string()))
                    };
                }
                AgentEvent::Error { message } => {
                    status = ExecStatus::Error(message);
                }
                AgentEvent::ConnectionError { message } => {
                    status = ExecStatus::Error(format!("Connection error: {}", message));
                }
                AgentEvent::CommandResponse { .. } => {}
            }

            // 每 1.5 秒更新一次或狀態改變時（工業頻率）
            if last_upd.elapsed() >= std::time::Duration::from_millis(1500) || status != ExecStatus::Running
            {
                let mut embed = CreateEmbed::new();
                let i18n = state.i18n.read().await;
                let desc = composer.render();

                match &status {
                    ExecStatus::Error(e) => {
                        embed = embed.title(i18n.get("api_error")).color(0xff0000).description(format!("{}\n\n❌ **錯誤:** {}", desc, e));
                    }
                    ExecStatus::Aborted => {
                        embed = embed.title(i18n.get("user_aborted")).color(0xff0000).description(format!("{}\n\n⚠️ {}", desc, i18n.get("aborted_desc")));
                    }
                    ExecStatus::Success => {
                        embed = embed.title(i18n.get("pi_response")).color(0x00ff00).description(desc);
                    }
                    ExecStatus::Running => {
                        embed = embed.title(i18n.get("pi_working")).color(0xFFA500).description(if desc.is_empty() { i18n.get("wait") } else { desc });
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

        if !typing_task.is_finished() {
            typing_task.abort();
        }
    }
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        info!("✅ Connected as {}!", ready.user.name);

        let command_list = commands::get_all_commands();
        let command_count = command_list.len();
        let commands: Vec<_> = command_list
            .into_iter()
            .map(|cmd| cmd.create_command())
            .collect();

        if let Err(e) = serenity::all::Command::set_global_commands(&ctx.http, commands).await {
            error!("Failed to register commands: {}", e);
        } else {
            info!("✅ Registered {} slash commands", command_count);
        }
    }

    async fn message(&self, ctx: Context, msg: Message) {
        if msg.author.bot {
            return;
        }

        let user_id = msg.author.id.to_string();
        let channel_id_str = msg.channel_id.to_string();

        let (is_auth, mention_only) = self.state.auth.is_authorized(&user_id, &channel_id_str);

        if !is_auth {
            let is_dm = msg.guild_id.is_none();
            let mentioned = msg.mentions_me(&ctx).await.unwrap_or(false);

            if is_dm || mentioned {
                let target_id = if is_dm { &user_id } else { &channel_id_str };
                match self
                    .state
                    .auth
                    .create_token(if is_dm { "user" } else { "channel" }, target_id)
                {
                    Ok(token) => {
                        let _ = msg
                            .reply(
                                &ctx.http,
                                format!(
                                    "🔒 需要認證！\n在伺服器終端機執行:\n`agent-discord auth {}`\n(5 分鐘內有效)",
                                    token
                                ),
                            )
                            .await;
                    }
                    Err(e) => {
                        warn!("Failed to generate auth token: {}", e);
                    }
                }
            }
            return;
        }

        if mention_only && !msg.mentions_me(&ctx).await.unwrap_or(false) && msg.guild_id.is_some() {
            return;
        }

        let channel_id = msg.channel_id.get();

        let channel_config = match ChannelConfig::load().await {
            Ok(c) => c,
            Err(e) => {
                error!("Failed to load channel config: {}", e);
                return;
            }
        };
        let agent_type = channel_config.get_agent_type(&channel_id_str);

        let agent = match self
            .state
            .session_manager
            .get_or_create_session(channel_id, agent_type.clone())
            .await
        {
            Ok(a) => a,
            Err(e) => {
                error!("Failed to create agent session: {}", e);
                let _ = msg
                    .reply(
                        &ctx.http,
                        format!("❌ **系統錯誤**: 無法初始化 {} session\n詳情: `{}`", agent_type, e),
                    )
                    .await;
                return;
            }
        };

        let content = if msg.content.starts_with('!') {
            &msg.content[1..]
        } else {
            &msg.content
        }
        .to_string();

        let handler_state = self.state.clone();
        tokio::spawn(async move {
            Handler::start_agent_loop(
                agent,
                ctx.http.clone(),
                msg.channel_id,
                handler_state,
                Some(content),
            )
            .await;
        });
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        match interaction {
            Interaction::Command(command) => {
                self.handle_slash_command(&ctx, &command).await;
            }
            Interaction::Component(component) => {
                let custom_id = component.data.custom_id.as_str();
                
                if custom_id.starts_with("agent_") {
                    if let Err(e) = handle_button(&ctx, &component, &self.state).await {
                        error!("Failed to handle agent button: {}", e);
                    }
                } else if custom_id == "model_select" {
                    let channel_id_str = component.channel_id.to_string();
                    let channel_config = match ChannelConfig::load().await {
                        Ok(c) => c,
                        Err(e) => {
                            error!("Failed to load channel config: {}", e);
                            return;
                        }
                    };
                    let agent_type = channel_config.get_agent_type(&channel_id_str);
                    
                    match self
                        .state
                        .session_manager
                        .get_or_create_session(component.channel_id.get(), agent_type)
                        .await
                    {
                        Ok(agent) => {
                            if let Err(e) = commands::model::handle_model_select(&ctx, &component, agent).await {
                                error!("Failed to handle model select: {}", e);
                            }
                        }
                        Err(e) => {
                            error!("Failed to get agent for model select: {}", e);
                            let _ = component
                                .create_response(
                                    &ctx.http,
                                    CreateInteractionResponse::Message(
                                        CreateInteractionResponseMessage::new()
                                            .content(format!("❌ 無法連線至 agent: {}", e))
                                            .ephemeral(true),
                                    ),
                                )
                                .await;
                        }
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
        let channel_id_str = command.channel_id.to_string();

        if cmd_name == "agent" {
            for cmd in commands::get_all_commands() {
                if cmd.name() == cmd_name {
                    if let Err(e) = cmd.execute(ctx, command, Arc::new(NoOpAgent)).await {
                        error!("Command {} failed: {}", cmd_name, e);
                    }
                    return;
                }
            }
            return;
        }

        let channel_config = match ChannelConfig::load().await {
            Ok(c) => c,
            Err(e) => {
                error!("Failed to load channel config: {}", e);
                return;
            }
        };
        let agent_type = channel_config.get_agent_type(&channel_id_str);

        let agent = match self
            .state
            .session_manager
            .get_or_create_session(command.channel_id.get(), agent_type)
            .await
        {
            Ok(a) => a,
            Err(e) => {
                error!("Failed to create agent session: {}", e);
                let _ = command
                    .create_response(
                        &ctx.http,
                        CreateInteractionResponse::Message(
                            CreateInteractionResponseMessage::new()
                                .content(format!("❌ 無法連線至 agent: {}", e))
                                .ephemeral(true),
                        ),
                    )
                    .await;
                return;
            }
        };

        for cmd in commands::get_all_commands() {
            if cmd.name() == cmd_name {
                if let Err(e) = cmd.execute(ctx, command, agent).await {
                    error!("Command {} failed: {}", cmd_name, e);
                }
                return;
            }
        }
    }
}

async fn run_bot() -> anyhow::Result<()> {
    migrate::run_migrations().await?;
    let config = Arc::new(Config::load().await?);
    let log_level = match config.debug_level.as_deref() {
        Some("DEBUG") => Level::DEBUG,
        Some("TRACE") => Level::TRACE,
        _ => Level::INFO,
    };
    tracing_subscriber::fmt().with_max_level(log_level).init();

    let i18n = Arc::new(RwLock::new(I18n::new(&config.language)));
    let auth = Arc::new(AuthManager::new());
    let session_manager = Arc::new(SessionManager::new(config.clone()));

    let state = AppState {
        config,
        session_manager,
        auth,
        i18n,
    };

    let token = state.config.discord_token.clone();
    let handler = Handler { state };
    let mut client = Client::builder(
        &token,
        GatewayIntents::GUILD_MESSAGES
            | GatewayIntents::MESSAGE_CONTENT
            | GatewayIntents::GUILDS
            | GatewayIntents::DIRECT_MESSAGES,
    )
    .event_handler(handler)
    .await?;

    client.start().await?;
    Ok(())
}

fn manage_daemon(action: DaemonAction) -> anyhow::Result<()> {
    let service_name = "agent-discord-rs";
    let base_dir = migrate::get_base_dir();
    let systemd_dir = base_dir.join("systemd");
    let service_file = systemd_dir.join(format!("{}.service", service_name));

    match action {
        DaemonAction::Enable => {
            std::fs::create_dir_all(&systemd_dir)?;
            let exe_path = std::env::current_exe()?;

            let content = format!(
                r#"[Unit]
Description=Agent Discord RS
After=network.target

[Service]
Type=simple
ExecStart={} run
Restart=on-failure
RestartSec=5s

[Install]
WantedBy=default.target
"#,
                exe_path.display()
            );

            std::fs::write(&service_file, content)?;
            let user_systemd = dirs::home_dir().unwrap().join(".config/systemd/user");
            std::fs::create_dir_all(&user_systemd)?;
            let link = user_systemd.join(format!("{}.service", service_name));
            if link.exists() { std::fs::remove_file(&link)?; }
            std::os::unix::fs::symlink(&service_file, &link)?;

            StdCommand::new("systemctl").arg("--user").arg("daemon-reload").status()?;
            StdCommand::new("systemctl").arg("--user").arg("enable").arg(service_name).status()?;
            StdCommand::new("systemctl").arg("--user").arg("start").arg(service_name).status()?;
        }
        DaemonAction::Disable => {
            let _ = StdCommand::new("systemctl").arg("--user").arg("stop").arg(service_name).status();
            let _ = StdCommand::new("systemctl").arg("--user").arg("disable").arg(service_name).status();
            let user_systemd = dirs::home_dir().unwrap().join(".config/systemd/user");
            let link = user_systemd.join(format!("{}.service", service_name));
            if link.exists() { std::fs::remove_file(&link)?; }
            if service_file.exists() { std::fs::remove_file(&service_file)?; }
            let _ = StdCommand::new("systemctl").arg("--user").arg("daemon-reload").status();
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
            let res = StdCommand::new("systemctl").arg("--user").arg("kill").arg("-s").arg("HUP").arg("agent-discord-rs").status();
            match res {
                Ok(status) if status.success() => println!("✅ Reload signal sent successfully."),
                _ => eprintln!("❌ Failed to send reload signal."),
            }
        }
        Some(Commands::Auth { token }) => {
            let manager = AuthManager::new();
            match manager.redeem_token(&token) {
                Ok((type_, id)) => println!("✅ Successfully authorized {} {}.", type_, id),
                Err(e) => eprintln!("❌ Authorization failed: {}", e),
            }
        }
        Some(Commands::Version) => println!("agent-discord v{}", env!("CARGO_PKG_VERSION")),
        None => {
            use clap::CommandFactory;
            Cli::command().print_help()?;
        }
    }
    Ok(())
}
