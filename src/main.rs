use agent::{AiAgent, UserInput};
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
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn, Level};

mod cron;
mod i18n;

mod agent;
mod auth;
mod commands;
mod composer;
mod config;
mod flow;
mod migrate;
mod session;
mod uploads;
mod writer_logic;

use auth::AuthManager;
use commands::agent::{handle_button, ChannelConfig};
use composer::EmbedComposer;
use config::Config;
use cron::CronManager;
use flow::{
    build_render_view, build_systemd_service_content, detect_timezone, get_systemd_service_path,
    resolve_channel_assistant_name, route_component, route_modal, should_process_message,
    ComponentRoute, ModalRoute,
};
use i18n::I18n;
use session::SessionManager;
use uploads::UploadManager;
use writer_logic::apply_agent_event;

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
type PendingInputMap = HashMap<u64, UserInput>;
type QueuedLoopRequest = (u64, UserInput);

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub session_manager: Arc<SessionManager>,
    pub auth: Arc<AuthManager>,
    pub i18n: Arc<RwLock<I18n>>,
    pub backend_manager: Arc<agent::manager::BackendManager>,
    pub cron_manager: Arc<CronManager>,
    pub active_renders: Arc<Mutex<ActiveRenderMap>>,
    pub pending_inputs: Arc<Mutex<PendingInputMap>>,
    pub queued_loop_tx: mpsc::UnboundedSender<QueuedLoopRequest>,
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

fn should_auto_recover_request_error(agent_type: &str, error_text: &str) -> bool {
    if agent_type != "kilo" && agent_type != "opencode" {
        return false;
    }

    let lower = error_text.to_lowercase();
    lower.contains("error sending request for url")
        || lower.contains("connection refused")
        || lower.contains("tcp connect error")
        || lower.contains("connection reset")
        || lower.contains("broken pipe")
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
        let mut initial_input = initial_input;

        // 1. 若該頻道已有執行中任務，將新輸入排隊（覆蓋舊排隊）而不是硬中止。
        {
            let has_active = {
                let active = state.active_renders.lock().await;
                active.contains_key(&channel_id_u64)
            };
            if has_active {
                if let Some(input) = initial_input.take() {
                    let mut pending = state.pending_inputs.lock().await;
                    pending.insert(channel_id_u64, input);
                    info!(
                        "⏳ Queued input for channel {} while render is running",
                        channel_id_u64
                    );
                }
                return;
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
            resolve_channel_assistant_name(
                &channel_cfg,
                &channel_id.to_string(),
                &state.config.assistant_name,
            )
        };

        // --- 任務啟動：收集所有 Handles ---
        let mut handles = Vec::new();

        let prompt_input = if let Some(mut input) = initial_input {
            let mut final_msg = input.text;
            if is_brand_new {
                let prompts = load_all_prompts();
                if !prompts.is_empty() {
                    final_msg = format!("{}\n\n{}", prompts, final_msg);
                }
            }
            input.text = final_msg;
            Some(input)
        } else {
            None
        };

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
                    let i18n = render_i18n.read().await;
                    let (title, color, body) =
                        build_render_view(&i18n, &current_status, &desc, &render_assistant_name);
                    let embed = CreateEmbed::new()
                        .title(title)
                        .color(color)
                        .description(body);

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
                    let mut should_start_queued = false;
                    // 完工：從活躍任務中移除自己
                    let mut active = render_state.active_renders.lock().await;
                    if let Some((active_msg_id, _)) = active.get(&channel_id_u64) {
                        if *active_msg_id == render_msg_id {
                            active.remove(&channel_id_u64);
                            should_start_queued = true;
                            info!(
                                "✅ Completed response registered as historical for channel {}",
                                channel_id_u64
                            );
                        }
                    }
                    drop(active);

                    if should_start_queued {
                        let next_input = {
                            let mut pending = render_state.pending_inputs.lock().await;
                            pending.remove(&channel_id_u64)
                        };
                        if let Some(next_input) = next_input {
                            if let Err(e) = render_state
                                .queued_loop_tx
                                .send((channel_id_u64, next_input))
                            {
                                warn!(
                                    "⚠️ Failed to dispatch queued input for channel {}: {}",
                                    channel_id_u64, e
                                );
                            }
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
        let writer_agent_type = agent.agent_type().to_string();
        let writer_task = tokio::spawn(async move {
            loop {
                match tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv()).await {
                    Ok(Ok(event)) => {
                        let mut comp = writer_composer.lock().await;
                        let mut s = writer_status.lock().await;
                        let finished = apply_agent_event(&mut comp, &mut s, event);
                        if finished && *s == ExecStatus::Success && comp.blocks.is_empty() {
                            warn!(
                                "⚠️ Empty success response detected: channel={}, agent={}",
                                channel_id_u64, writer_agent_type
                            );
                        }
                        drop(comp);
                        drop(s);
                        if finished {
                            break;
                        }
                    }
                    Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(n))) => {
                        info!("⚠️ Writer lagged by {} messages", n);
                        continue;
                    }
                    Ok(Err(_)) => break,
                    Err(_) => {
                        let s = writer_status.lock().await;
                        if *s != ExecStatus::Running {
                            break;
                        }
                    }
                }
                tokio::task::yield_now().await;
            }
        });

        if let Some(input) = prompt_input {
            let agent_for_prompt = Arc::clone(&agent);
            let status_for_prompt = Arc::clone(&status);
            let composer_for_prompt = Arc::clone(&composer);
            let state_for_prompt = state.clone();
            let prompt_agent_type = agent.agent_type().to_string();
            // Detach the prompt task from the abortable display-task handles.
            // When /abort fires it only kills render_task + writer_task (the UI
            // tasks).  The prompt task continues in the background so the
            // underlying backend (especially Copilot, which has no abort API)
            // finishes naturally before the next prompt is dispatched.
            // For Copilot the prompt_lock in CopilotRuntime serialises this.
            tokio::spawn(async move {
                if let Err(e) = agent_for_prompt.prompt_with_input(&input).await {
                    let err_text = e.to_string();
                    let recoverable_request_error =
                        should_auto_recover_request_error(&prompt_agent_type, &err_text);
                    let mut has_no_stream_output = {
                        let comp = composer_for_prompt.lock().await;
                        comp.blocks.is_empty()
                    };
                    if recoverable_request_error && has_no_stream_output {
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                        has_no_stream_output = {
                            let comp = composer_for_prompt.lock().await;
                            comp.blocks.is_empty()
                        };
                        if !has_no_stream_output {
                            info!(
                                "⚠️ POST prompt reported recoverable error: {}, but stream became active. Continuing...",
                                err_text
                            );
                            return;
                        }
                    }

                    let mut queued_recovery = false;
                    if has_no_stream_output && recoverable_request_error {
                        let is_still_running = {
                            let s = status_for_prompt.lock().await;
                            *s == ExecStatus::Running
                        };
                        if !is_still_running {
                            return;
                        }
                        state_for_prompt
                            .session_manager
                            .remove_session(channel_id_u64)
                            .await;
                        let mut pending = state_for_prompt.pending_inputs.lock().await;
                        pending
                            .entry(channel_id_u64)
                            .or_insert_with(|| input.clone());
                        queued_recovery = true;
                        warn!(
                            "♻️ Auto-recovery queued for channel {} ({}) due to backend request failure: {}",
                            channel_id_u64, prompt_agent_type, err_text
                        );
                    }

                    let mut s = status_for_prompt.lock().await;
                    if *s == ExecStatus::Running {
                        if has_no_stream_output {
                            if queued_recovery {
                                *s = ExecStatus::Error(
                                    "Backend temporary failure, auto-retrying...".to_string(),
                                );
                            } else {
                                *s = ExecStatus::Error(err_text);
                            }
                        } else {
                            info!("⚠️ POST prompt reported error: {}, but SSE stream is active. Continuing...", e);
                        }
                    }
                }
            });
        }

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
        let mentioned = msg.mentions_me(&ctx).await.unwrap_or(false);
        if !should_process_message(msg.author.bot, msg.kind, false, mentioned) {
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
            if mentioned {
                if let Ok(token) = self.state.auth.create_token("channel", &channel_id_str) {
                    let auth_msg = {
                        let i18n = self.state.i18n.read().await;
                        i18n.get_args("auth_required_cmd", &[token])
                    };
                    let _ = msg.reply(&ctx.http, auth_msg).await;
                }
            }
            return;
        }

        if !should_process_message(false, msg.kind, mention_only, mentioned) {
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
            match route_modal(custom_id) {
                ModalRoute::CronSetup => {
                    let state = self.state.clone();
                    tokio::spawn(async move {
                        let _ = commands::cron::handle_modal_submit(&ctx, &modal, &state).await;
                    });
                }
                ModalRoute::ConfigAssistant => {
                    let state = self.state.clone();
                    tokio::spawn(async move {
                        let _ =
                            commands::config::handle_assistant_modal_submit(&ctx, &modal, &state)
                                .await;
                    });
                }
                ModalRoute::Ignore => {}
            }
        } else if let Interaction::Component(component) = interaction {
            let custom_id = component.data.custom_id.as_str();
            match route_component(custom_id) {
                ComponentRoute::Config => {
                    let _ =
                        commands::config::handle_config_select(&ctx, &component, &self.state).await;
                }
                ComponentRoute::Agent => {
                    let _ = handle_button(&ctx, &component, &self.state).await;
                }
                ComponentRoute::CronDelete => {
                    let state = self.state.clone();
                    tokio::spawn(async move {
                        let _ =
                            commands::cron::handle_delete_select(&ctx, &component, &state).await;
                    });
                }
                ComponentRoute::ModelSelect => {
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
                            let _ = commands::model::handle_model_select(
                                &ctx, &component, agent, &state,
                            )
                            .await;
                        }
                    });
                }
                ComponentRoute::Ignore => {}
            }
        }
    }
}

async fn run_bot() -> anyhow::Result<()> {
    migrate::run_migrations().await?;
    let config = Arc::new(Config::load().await?);
    let cron_manager = Arc::new(CronManager::new().await?);
    let (queued_loop_tx, mut queued_loop_rx) = mpsc::unbounded_channel::<QueuedLoopRequest>();
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
        pending_inputs: Arc::new(Mutex::new(HashMap::new())),
        queued_loop_tx,
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

    let queue_state = state.clone();
    let queue_http = client.http.clone();
    tokio::spawn(async move {
        while let Some((channel_id_u64, input)) = queued_loop_rx.recv().await {
            let channel_id = serenity::model::id::ChannelId::from(channel_id_u64);
            let channel_id_str = channel_id.to_string();
            let channel_config = ChannelConfig::load().await.unwrap_or_default();
            let agent_type = channel_config.get_agent_type(&channel_id_str);
            match queue_state
                .session_manager
                .get_or_create_session(channel_id_u64, agent_type, &queue_state.backend_manager)
                .await
            {
                Ok((agent, is_new)) => {
                    Handler::start_agent_loop(
                        agent,
                        queue_http.clone(),
                        channel_id,
                        (*queue_state).clone(),
                        Some(input),
                        is_new,
                    )
                    .await;
                }
                Err(e) => error!("❌ Failed to run queued input: {}", e),
            }
        }
    });

    // 初始化 CronManager 的執行環境
    state
        .cron_manager
        .init(client.http.clone(), Arc::downgrade(&state))
        .await;

    client.start().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::load_all_prompts;
    use crate::migrate::{get_prompts_dir, BASE_DIR_ENV};
    use std::sync::{Mutex, OnceLock};
    use tempfile::tempdir;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn test_load_all_prompts_creates_defaults_when_empty() {
        let _guard = env_lock().lock().expect("lock");
        let dir = tempdir().expect("tempdir");
        // SAFETY: serialized by env lock
        unsafe { std::env::set_var(BASE_DIR_ENV, dir.path()) };

        let out = load_all_prompts();
        assert!(!out.trim().is_empty());
        assert!(dir.path().join("prompts").exists());

        // SAFETY: serialized by env lock
        unsafe { std::env::remove_var(BASE_DIR_ENV) };
    }

    #[test]
    fn test_load_all_prompts_reads_existing_files_sorted() {
        let _guard = env_lock().lock().expect("lock");
        let dir = tempdir().expect("tempdir");
        // SAFETY: serialized by env lock
        unsafe { std::env::set_var(BASE_DIR_ENV, dir.path()) };

        let prompts_dir = get_prompts_dir();
        std::fs::create_dir_all(&prompts_dir).expect("create prompts dir");
        std::fs::write(prompts_dir.join("b.md"), "B").expect("write b");
        std::fs::write(prompts_dir.join("a.md"), "A").expect("write a");

        let out = load_all_prompts();
        assert_eq!(out, "A\n\nB");

        // SAFETY: serialized by env lock
        unsafe { std::env::remove_var(BASE_DIR_ENV) };
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();
    let cli = Cli::parse();
    match cli.command {
        Some(Commands::Run) => run_bot().await?,
        Some(Commands::Version) => println!("v{}", env!("CARGO_PKG_VERSION")),
        Some(Commands::Daemon { action }) => {
            let service_path = get_systemd_service_path()?;

            match action {
                DaemonAction::Enable => {
                    // 1. 偵測目前執行檔路徑
                    let exe_path = std::env::current_exe()?.to_string_lossy().to_string();

                    // 2. 偵測時區
                    let tz = detect_timezone();

                    // 3. 取得目前環境變數
                    let current_path = std::env::var("PATH").unwrap_or_default();
                    let augmented_path = agent::runtime::build_augmented_path(&current_path);

                    let service_content =
                        build_systemd_service_content(&exe_path, &augmented_path, &tz);

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
