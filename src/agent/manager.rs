use crate::agent::AgentType;
use crate::agent::runtime;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

pub struct BackendProcess {
    pub child: Mutex<Child>,
    pub port: u16,
}

pub struct BackendManager {
    processes: Arc<Mutex<HashMap<String, Arc<BackendProcess>>>>,
    config: Arc<crate::config::Config>,
}

impl BackendManager {
    pub fn new(config: Arc<crate::config::Config>) -> Self {
        Self {
            processes: Arc::new(Mutex::new(HashMap::new())),
            config,
        }
    }

    fn spawn_stream_logger<R>(label: String, reader: R)
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
    {
        tokio::spawn(async move {
            let mut r = BufReader::new(reader);
            let mut line = String::new();
            while let Ok(n) = r.read_line(&mut line).await {
                if n == 0 {
                    break;
                }
                let msg = line.trim();
                if !msg.is_empty() {
                    warn!("{}: {}", label, msg);
                }
                line.clear();
            }
        });
    }

    fn get_free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .map(|addr| addr.port())
            .unwrap_or(40000)
    }

    pub async fn ensure_backend(&self, agent_type: &AgentType) -> anyhow::Result<u16> {
        let key = agent_type.to_string();

        // 1. 快速檢查是否已有運行的進程 (使用最小鎖定範圍)
        let mut dead_backend = false;
        {
            let procs = self.processes.lock().await;
            if let Some(p) = procs.get(&key) {
                let mut child = p.child.lock().await;
                if let Ok(None) = child.try_wait() {
                    return Ok(p.port);
                }
                dead_backend = true;
            }
        }

        if dead_backend {
            let mut procs = self.processes.lock().await;
            warn!("Backend {} died. Removing from map.", agent_type);
            procs.remove(&key);
        }

        // 2. 啟動新進程 (重新加鎖)
        let mut procs = self.processes.lock().await;
        // 再次檢查 (Double-checked locking)
        if let Some(p) = procs.get(&key) {
            return Ok(p.port);
        }

        let port = Self::get_free_port();
        let bin_name = match agent_type {
            AgentType::Kilo => "kilo",
            AgentType::Opencode => "opencode",
            _ => return Err(anyhow::anyhow!("Unsupported agent type")),
        };

        let env_key = match agent_type {
            AgentType::Opencode => "OPENCODE_BINARY",
            AgentType::Kilo => "KILO_BINARY",
            _ => "",
        };
        let resolved_path = if env_key.is_empty() {
            runtime::resolve_binary_path(bin_name)
        } else {
            runtime::resolve_binary_with_env(env_key, bin_name)
        };
        info!(
            "🚀 Starting {} on port {} from {}",
            agent_type, port, resolved_path
        );

        let mut cmd = Command::new(&resolved_path);
        cmd.arg("serve")
            .arg("--port")
            .arg(port.to_string())
            .arg("--hostname")
            .arg("127.0.0.1")
            .env("NODE_OPTIONS", "--max-old-space-size=4096"); // 透過環境變數限制封裝後的 Node.js 內存

        let current_path = std::env::var("PATH").unwrap_or_default();
        let new_path = runtime::build_augmented_path(&current_path);
        cmd.env("PATH", new_path);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        if let Some(password) = &self.config.opencode.password {
            if !password.is_empty() {
                match agent_type {
                    AgentType::Opencode => {
                        cmd.env("OPENCODE_SERVER_PASSWORD", password);
                    }
                    AgentType::Kilo => {
                        cmd.env("KILO_SERVER_PASSWORD", password);
                    }
                    _ => {}
                }
            }
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow::anyhow!("Spawn failed: {}", e))?;
        if let Some(stdout) = child.stdout.take() {
            Self::spawn_stream_logger(format!("{}(stdout)", agent_type), stdout);
        }
        if let Some(stderr) = child.stderr.take() {
            Self::spawn_stream_logger(format!("{}(stderr)", agent_type), stderr);
        }
        let process = Arc::new(BackendProcess {
            child: Mutex::new(child),
            port,
        });
        procs.insert(key, process);

        // 3. 等待健康檢查 (釋放鎖定，避免阻塞其他頻道)
        drop(procs);

        let mut attempts = 0;
        let client = reqwest::Client::new();
        let health_url = format!("http://127.0.0.1:{}/provider", port);

        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let mut req = client.get(&health_url);
            if let Some(password) = &self.config.opencode.password {
                if !password.is_empty() {
                    req = req.header("Authorization", format!("Bearer {}", password));
                }
            }

            match req.send().await {
                Ok(resp) if resp.status().is_success() => {
                    info!("✅ Backend {} is ready on port {}", agent_type, port);
                    return Ok(port);
                }
                _ => {
                    attempts += 1;
                    if attempts > 60 {
                        error!("❌ Backend {} failed to start on port {}", agent_type, port);
                        return Err(anyhow::anyhow!("Backend timeout"));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::BackendManager;
    use crate::agent::AgentType;
    use crate::config::Config;
    use std::sync::Arc;

    #[test]
    fn test_get_free_port_returns_non_zero() {
        let p = BackendManager::get_free_port();
        assert!(p > 0);
    }

    #[tokio::test]
    async fn test_ensure_backend_rejects_unsupported_agent_type() {
        let manager = BackendManager::new(Arc::new(Config::default()));
        let err = manager
            .ensure_backend(&AgentType::Pi)
            .await
            .expect_err("pi should be unsupported in backend manager");
        assert!(err.to_string().contains("Unsupported agent type"));
    }
}
