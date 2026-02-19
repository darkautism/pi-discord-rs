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
    pub id: Uuid, // 這是我們自定義的 ID，用於索引
    #[serde(default)]
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
        let base_dir = crate::migrate::get_base_dir();
        Self::with_config_dir(base_dir).await
    }

    pub async fn with_config_dir(config_dir: PathBuf) -> anyhow::Result<Self> {
        let scheduler = JobScheduler::new().await?;
        scheduler.start().await?;

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

        let local_now = chrono::Local::now();
        let utc_now = chrono::Utc::now();
        info!(
            "📅 CronManager initialized. Local: {}, UTC: {}",
            local_now, utc_now
        );
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

        let job = Job::new_async_tz(cron_expr.as_str(), chrono::Local, move |_uuid, _l| {
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
        let removed_scheduler_id = {
            let mut jobs = self.jobs.lock().await;
            jobs.remove(&id).and_then(|info| info.scheduler_id)
        };

        if let Some(s_id) = removed_scheduler_id {
            self.scheduler.remove(&s_id).await?;
            info!("🗑️ Removed cron job {} (scheduler id: {})", id, s_id);
        }

        self.save_to_disk().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::{tempdir, TempDir};

    async fn new_test_manager(dir: &TempDir) -> anyhow::Result<CronManager> {
        CronManager::with_config_dir(dir.path().to_path_buf()).await
    }

    fn build_job(job_id: Uuid, channel_id: u64, prompt: &str) -> CronJobInfo {
        CronJobInfo {
            id: job_id,
            scheduler_id: None,
            channel_id,
            cron_expr: "0 * * * * *".to_string(),
            prompt: prompt.to_string(),
            creator_id: 1,
            description: "test".to_string(),
        }
    }

    #[tokio::test]
    async fn test_cron_persistence() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let manager = new_test_manager(&dir).await?;

        let job_id = Uuid::new_v4();
        let mut info = build_job(job_id, 12345, "Test Prompt");
        info.cron_expr = "0 0 * * * *".to_string();
        info.creator_id = 67890;
        info.description = "Test Description".to_string();

        // Add job
        manager.add_job(info).await?;

        // Check if file exists
        let path = dir.path().join("cron_jobs.json");
        assert!(path.exists());

        // Create a new manager instance to load
        let manager2 = new_test_manager(&dir).await?;
        manager2.load_from_disk().await?;

        let jobs = manager2.jobs.lock().await;
        assert_eq!(jobs.len(), 1);
        let loaded = jobs.get(&job_id).expect("job should be persisted");
        assert_eq!(loaded.prompt, "Test Prompt");
        assert!(loaded.scheduler_id.is_some());

        Ok(())
    }

    #[tokio::test]
    async fn test_remove_job_updates_memory_and_disk() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let manager = new_test_manager(&dir).await?;
        let job_id = Uuid::new_v4();
        let mut info = build_job(job_id, 99999u64, "Test Trigger");
        info.cron_expr = "1/1 * * * * *".to_string();
        info.creator_id = 111;

        manager.add_job(info).await?;
        manager.remove_job(job_id).await?;

        let jobs = manager.jobs.lock().await;
        assert!(!jobs.contains_key(&job_id));
        drop(jobs);

        let manager2 = new_test_manager(&dir).await?;
        manager2.load_from_disk().await?;
        let jobs2 = manager2.jobs.lock().await;
        assert!(!jobs2.contains_key(&job_id));

        Ok(())
    }

    #[tokio::test]
    async fn test_get_jobs_for_channel_filters_correctly() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let manager = new_test_manager(&dir).await?;
        let channel_a = 11111_u64;
        let channel_b = 22222_u64;
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();

        manager.add_job(build_job(id_a, channel_a, "A")).await?;
        let mut job_b = build_job(id_b, channel_b, "B");
        job_b.creator_id = 2;
        manager.add_job(job_b).await?;

        let jobs_a = manager.get_jobs_for_channel(channel_a).await;
        assert_eq!(jobs_a.len(), 1);
        assert_eq!(jobs_a[0].id, id_a);
        let jobs_b = manager.get_jobs_for_channel(channel_b).await;
        assert_eq!(jobs_b.len(), 1);
        assert_eq!(jobs_b[0].id, id_b);

        Ok(())
    }
}
