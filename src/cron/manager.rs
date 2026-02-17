use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_cron_scheduler::{Job, JobScheduler};
use tracing::error;
use uuid::Uuid;

use crate::AppState;
use std::sync::Weak;

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct CronJobInfo {
    pub id: Uuid,
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
        scheduler.start().await?;

        let config_dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("agent-discord-rs");
        std::fs::create_dir_all(&config_dir)?;

        Ok(Self {
            scheduler,
            jobs: Arc::new(Mutex::new(HashMap::new())),
            config_dir,
            http: Arc::new(Mutex::new(None)),
            state: Arc::new(Mutex::new(None)),
        })
    }

    pub async fn init(&self, http: Arc<serenity::all::Http>, state: Weak<AppState>) {
        *self.http.lock().await = Some(http);
        *self.state.lock().await = Some(state);

        // 啟動時重新註冊所有已載入的任務
        let jobs: Vec<CronJobInfo> = {
            let jobs_map = self.jobs.lock().await;
            jobs_map.values().cloned().collect()
        };

        for info in jobs {
            let _ = self.register_job_to_scheduler(info).await;
        }
    }

    pub async fn add_job(&self, info: CronJobInfo) -> anyhow::Result<Uuid> {
        let id = info.id;

        // 1. 存入記憶體
        {
            let mut jobs = self.jobs.lock().await;
            jobs.insert(id, info.clone());
        }

        // 2. 註冊到排程器
        self.register_job_to_scheduler(info).await?;

        // 3. 存入磁碟
        self.save_to_disk().await?;

        Ok(id)
    }

    async fn register_job_to_scheduler(&self, info: CronJobInfo) -> anyhow::Result<()> {
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
                    error!("❌ Cron job triggered but Http/State not initialized");
                }
            })
        })?;

        self.scheduler.add(job).await?;
        Ok(())
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
        // 1. Remove from scheduler
        self.scheduler.remove(&id).await?;

        // 2. Remove from memory map
        {
            let mut jobs = self.jobs.lock().await;
            jobs.remove(&id);
        }

        // 3. Save updated list to disk
        self.save_to_disk().await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_cron_persistence() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let manager = CronManager {
            scheduler: JobScheduler::new().await?,
            jobs: Arc::new(Mutex::new(HashMap::new())),
            config_dir: dir.path().to_path_buf(),
            http: Arc::new(Mutex::new(None)),
            state: Arc::new(Mutex::new(None)),
        };
        manager.scheduler.start().await?;

        let job_id = Uuid::new_v4();
        let info = CronJobInfo {
            id: job_id,
            channel_id: 12345,
            cron_expr: "0 0 * * * *".to_string(), // Every hour
            prompt: "Test Prompt".to_string(),
            creator_id: 67890,
            description: "Test Description".to_string(),
        };

        // Add job
        manager.add_job(info).await?;

        // Check if file exists
        let path = dir.path().join("cron_jobs.json");
        assert!(path.exists());

        // Create a new manager instance to load
        let manager2 = CronManager {
            scheduler: JobScheduler::new().await?,
            jobs: Arc::new(Mutex::new(HashMap::new())),
            config_dir: dir.path().to_path_buf(),
            http: Arc::new(Mutex::new(None)),
            state: Arc::new(Mutex::new(None)),
        };
        manager2.load_from_disk().await?;

        let jobs = manager2.jobs.lock().await;
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs.get(&job_id).unwrap().prompt, "Test Prompt");

        Ok(())
    }
}
