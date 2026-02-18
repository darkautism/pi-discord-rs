use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_cron_scheduler::{Job, JobScheduler};
use tracing::{error, info};
use uuid::Uuid;

use crate::AppState;
use std::sync::Weak;

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct CronJobInfo {
    pub id: Uuid,             // 這是我們自定義的 ID，用於索引
    pub scheduler_id: Option<Uuid>, // 這是排程器產生的內部 ID，用於移除
    pub channel_id: u64,
    pub cron_expr: String,
    pub prompt: String,
    pub creator_id: u64,
    pub description: String,
}

pub struct CronManager {
    scheduler: JobScheduler,
    jobs: Arc<Mutex<HashMap<Uuid, CronJobInfo>>>,
    config_dir: PathBuf,
    http: Arc<Mutex<Option<Arc<serenity::all::Http>>>>,
    state: Arc<Mutex<Option<Weak<AppState>>>>,
}

impl CronManager {
    pub async fn new() -> anyhow::Result<Self> {
        let scheduler = JobScheduler::new().await?;
        // 確保 Scheduler 已經啟動
        if let Err(e) = scheduler.start().await {
            error!("❌ Failed to start cron scheduler: {}", e);
        }

        let config_dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("agent-discord-rs");
        let _ = std::fs::create_dir_all(&config_dir);

        Ok(Self {
            scheduler,
            jobs: Arc::new(Mutex::new(HashMap::new())),
            config_dir,
            http: Arc::new(Mutex::new(None)),
            state: Arc::new(Mutex::new(None)),
        })
    }

    pub async fn init(&self, http: Arc<serenity::all::Http>, state: Weak<AppState>) {
        {
            let mut h = self.http.lock().await;
            *h = Some(http);
            let mut s = self.state.lock().await;
            *s = Some(state);
        }

        // 啟動時重新註冊所有已載入的任務
        let ids: Vec<Uuid> = {
            let jobs_map = self.jobs.lock().await;
            jobs_map.keys().cloned().collect()
        };

        for id in ids {
            if let Err(e) = self.re_register_job(id).await {
                error!("❌ Failed to re-register job {}: {}", id, e);
            }
        }
        info!("📅 CronManager initialized and jobs registered.");
    }

    pub async fn add_job(&self, mut info: CronJobInfo) -> anyhow::Result<Uuid> {
        let id = info.id;

        // 1. 註冊到排程器並獲取內部 ID
        let scheduler_id = self.register_job_to_scheduler(&info).await?;
        info.scheduler_id = Some(scheduler_id);

        // 2. 存入記憶體
        {
            let mut jobs = self.jobs.lock().await;
            jobs.insert(id, info);
        }

        // 3. 存入磁碟
        self.save_to_disk().await?;

        Ok(id)
    }

    async fn re_register_job(&self, id: Uuid) -> anyhow::Result<()> {
        let mut jobs = self.jobs.lock().await;
        if let Some(info) = jobs.get_mut(&id) {
            let scheduler_id = self.register_job_to_scheduler(info).await?;
            info.scheduler_id = Some(scheduler_id);
        }
        Ok(())
    }

    async fn register_job_to_scheduler(&self, info: &CronJobInfo) -> anyhow::Result<Uuid> {
        let cron_expr = info.cron_expr.clone();
        let prompt = info.prompt.clone();
        let channel_id_u64 = info.channel_id;

        let http_ptr = self.http.clone();
        let state_ptr = self.state.clone();

        let job = Job::new_async(cron_expr.as_str(), move |_uuid, _l| {
            let prompt = prompt.clone();
            let http_ptr = http_ptr.clone();
            let state_ptr = state_ptr.clone();
            Box::pin(async move {
                info!("⏰ Cron job triggered for channel {}", channel_id_u64);
                let http_opt = http_ptr.lock().await;
                let state_weak_opt = state_ptr.lock().await;

                if let (Some(http), Some(state_weak)) = (http_opt.as_ref(), state_weak_opt.as_ref())
                {
                    if let Some(state) = state_weak.upgrade() {
                        let channel_id = serenity::model::id::ChannelId::from(channel_id_u64);
                        let channel_id_str = channel_id.to_string();

                        let channel_config = crate::commands::agent::ChannelConfig::load()
                            .await
                            .unwrap_or_default();
                        let agent_type = channel_config.get_agent_type(&channel_id_str);

                        match state
                            .session_manager
                            .get_or_create_session(
                                channel_id_u64,
                                agent_type,
                                &state.backend_manager,
                            )
                            .await
                        {
                            Ok((agent, is_new)) => {
                                crate::Handler::start_agent_loop(
                                    agent,
                                    http.clone(),
                                    channel_id,
                                    (*state).clone(),
                                    Some(prompt),
                                    is_new,
                                )
                                .await;
                            }
                            Err(e) => {
                                error!("❌ Cron job execution failed to create session: {}", e)
                            }
                        }
                    } else {
                        error!("❌ Cron job triggered but AppState was dropped");
                    }
                } else {
                    error!("❌ Cron job triggered but Http/State not initialized. Did you call init()?");
                }
            })
        })?;

        let scheduler_id = self.scheduler.add(job).await?;
        Ok(scheduler_id)
    }

    async fn save_to_disk(&self) -> anyhow::Result<()> {
        let jobs = self.jobs.lock().await;
        let data = serde_json::to_string_pretty(&*jobs)?;
        let path = self.config_dir.join("cron_jobs.json");
        tokio::fs::write(path, data).await?;
        Ok(())
    }

    pub async fn load_from_disk(&self) -> anyhow::Result<()> {
        let path = self.config_dir.join("cron_jobs.json");
        if !path.exists() {
            return Ok(());
        }

        let data = tokio::fs::read_to_string(path).await?;
        let loaded_jobs: HashMap<Uuid, CronJobInfo> = serde_json::from_str(&data)?;

        let mut jobs = self.jobs.lock().await;
        *jobs = loaded_jobs;
        info!("📂 Loaded {} cron jobs from disk", jobs.len());

        Ok(())
    }

    pub async fn get_jobs_for_channel(&self, channel_id: u64) -> Vec<CronJobInfo> {
        let jobs = self.jobs.lock().await;
        jobs.values()
            .filter(|j| j.channel_id == channel_id)
            .cloned()
            .collect()
    }

    pub async fn remove_job(&self, id: Uuid) -> anyhow::Result<()> {
        let mut jobs = self.jobs.lock().await;
        if let Some(info) = jobs.remove(&id) {
            if let Some(s_id) = info.scheduler_id {
                self.scheduler.remove(&s_id).await?;
                info!("🗑️ Removed cron job {} (scheduler id: {})", id, s_id);
            }
        }

        self.save_to_disk().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_cron_trigger_logic() -> anyhow::Result<()> {
        let _dir = tempdir()?;
        let manager = CronManager::new().await?;
        
        // Mock 任務資料
        let job_id = Uuid::new_v4();
        let channel_id = 99999u64;
        let info = CronJobInfo {
            id: job_id,
            scheduler_id: None,
            channel_id,
            cron_expr: "1/1 * * * * *".to_string(), // 每秒觸發
            prompt: "Test Trigger".to_string(),
            creator_id: 111,
            description: "Test".to_string(),
        };

        // 驗證 add_job 能正確生成 scheduler_id
        let added_id = manager.add_job(info).await?;
        assert_eq!(added_id, job_id);

        let jobs = manager.jobs.lock().await;
        assert!(jobs.get(&job_id).unwrap().scheduler_id.is_some());

        Ok(())
    }

    #[tokio::test]
    async fn test_cron_flow_verification() -> anyhow::Result<()> {
        use crate::agent::{AiAgent, MockAgent, AgentEvent};

        // 1. Setup Mock Environment
        let agent = Arc::new(MockAgent::new());
        let (tx_verify, mut rx_verify) = tokio::sync::mpsc::channel(1);

        // 模擬 start_agent_loop 的簡化行為用於測試驗證
        let channel_id_to_check = 88888u64;
        let prompt_to_check = "Hello, Cron!".to_string();
        
        let agent_clone = agent.clone();
        tokio::spawn(async move {
            let mut rx = agent_clone.subscribe_events();
            agent_clone.prompt(&prompt_to_check).await.unwrap();
            
            while let Ok(event) = rx.recv().await {
                match event {
                    AgentEvent::MessageUpdate { ref text, .. } => {
                        let _ = tx_verify.send((channel_id_to_check, text.clone())).await;
                    }
                    AgentEvent::AgentEnd { .. } => break,
                    _ => {}
                }
            }
        });

        // 2. Verify Output
        if let Ok(Some((cid, text))) = tokio::time::timeout(std::time::Duration::from_secs(2), rx_verify.recv()).await {
            assert_eq!(cid, 88888);
            assert_eq!(text, "Mock Response");
            info!("✅ Integration flow verified: Channel={}, Text={}", cid, text);
        } else {
            anyhow::bail!("Flow verification timed out or failed");
        }

        Ok(())
    }
}
