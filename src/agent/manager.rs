use crate::agent::AgentType;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
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

    fn collect_candidate_bin_dirs() -> Vec<String> {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/kautism".to_string());
        let mut dirs = vec![
            format!("{}/.npm-global/bin", home),
            format!("{}/.opencode/bin", home),
            format!("{}/.local/bin", home),
            format!("{}/.volta/bin", home),
            "/usr/local/bin".to_string(),
            "/usr/bin".to_string(),
            "/snap/bin".to_string(),
        ];

        if let Ok(nvm_bin) = std::env::var("NVM_BIN") {
            dirs.push(nvm_bin);
        }

        let nvm_dir = std::env::var("NVM_DIR").unwrap_or_else(|_| format!("{}/.nvm", home));
        let node_versions_dir = Path::new(&nvm_dir).join("versions").join("node");
        if let Ok(entries) = std::fs::read_dir(node_versions_dir) {
            let mut version_bins = Vec::new();
            for entry in entries.flatten() {
                let p = entry.path().join("bin");
                if p.is_dir() {
                    version_bins.push(p.to_string_lossy().to_string());
                }
            }
            version_bins.sort();
            version_bins.reverse();
            dirs.extend(version_bins);
        }

        dirs
    }

    pub(crate) fn resolve_binary_path(bin: &str) -> String {
        if Path::new(bin).exists() {
            return bin.to_string();
        }

        for dir in Self::collect_candidate_bin_dirs() {
            let candidate = Path::new(&dir).join(bin);
            if candidate.exists() {
                return candidate.to_string_lossy().to_string();
            }
        }
        bin.to_string()
    }

    pub(crate) fn build_augmented_path(current_path: &str) -> String {
        let mut all = Self::collect_candidate_bin_dirs();
        all.push(current_path.to_string());
        all.join(":")
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

        let resolved_path = Self::resolve_binary_path(bin_name);
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
        let new_path = Self::build_augmented_path(&current_path);
        cmd.env("PATH", new_path);

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

        let child = cmd
            .spawn()
            .map_err(|e| anyhow::anyhow!("Spawn failed: {}", e))?;
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
