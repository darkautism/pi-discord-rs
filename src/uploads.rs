use crate::agent::UploadedFile;
use crate::migrate;
use serenity::all::Attachment;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::Mutex;
use tracing::{info, warn};
use uuid::Uuid;

pub struct UploadManager {
    client: reqwest::Client,
    root: PathBuf,
    max_file_bytes: u64,
    ttl: Duration,
    cleanup_interval: Duration,
    last_cleanup: Mutex<Option<Instant>>,
}

impl UploadManager {
    pub fn new(
        max_file_bytes: u64,
        ttl: Duration,
        cleanup_interval: Duration,
    ) -> anyhow::Result<Self> {
        let root = migrate::get_uploads_dir();
        std::fs::create_dir_all(&root)?;
        Ok(Self {
            client: reqwest::Client::new(),
            root,
            max_file_bytes,
            ttl,
            cleanup_interval,
            last_cleanup: Mutex::new(None),
        })
    }

    pub async fn stage_attachments(
        &self,
        channel_id: u64,
        attachments: &[Attachment],
    ) -> Vec<UploadedFile> {
        self.maybe_cleanup().await;

        if attachments.is_empty() {
            return Vec::new();
        }

        let mut out = Vec::new();
        for attachment in attachments {
            if attachment.size > self.max_file_bytes as u32 {
                warn!(
                    "Skipping attachment '{}' ({} bytes > max {} bytes)",
                    attachment.filename, attachment.size, self.max_file_bytes
                );
                continue;
            }

            match self.download_one(channel_id, attachment).await {
                Ok(file) => out.push(file),
                Err(e) => warn!(
                    "Failed to stage attachment '{}': {}",
                    attachment.filename, e
                ),
            }
        }

        out
    }

    async fn maybe_cleanup(&self) {
        let mut lock = self.last_cleanup.lock().await;
        let should_run = match *lock {
            Some(last) => last.elapsed() >= self.cleanup_interval,
            None => true,
        };

        if !should_run {
            return;
        }

        *lock = Some(Instant::now());
        drop(lock);

        if let Err(e) = self.cleanup_expired().await {
            warn!("Upload cleanup failed: {}", e);
        }
    }

    async fn cleanup_expired(&self) -> anyhow::Result<()> {
        let mut stack = vec![self.root.clone()];
        let now = SystemTime::now();
        let mut removed = 0usize;

        while let Some(dir) = stack.pop() {
            let mut entries = match tokio::fs::read_dir(&dir).await {
                Ok(v) => v,
                Err(_) => continue,
            };

            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                let metadata = entry.metadata().await?;

                if metadata.is_dir() {
                    stack.push(path);
                    continue;
                }

                let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                let age = now
                    .duration_since(modified)
                    .unwrap_or_else(|_| Duration::from_secs(0));

                if age > self.ttl {
                    if tokio::fs::remove_file(&path).await.is_ok() {
                        removed += 1;
                    }
                }
            }
        }

        self.remove_empty_dirs().await?;
        if removed > 0 {
            info!("🧹 Upload cleanup removed {} expired files", removed);
        }
        Ok(())
    }

    async fn remove_empty_dirs(&self) -> anyhow::Result<()> {
        let mut stack = vec![self.root.clone()];
        let mut dirs = Vec::new();

        while let Some(dir) = stack.pop() {
            dirs.push(dir.clone());
            let mut entries = match tokio::fs::read_dir(&dir).await {
                Ok(v) => v,
                Err(_) => continue,
            };
            while let Some(entry) = entries.next_entry().await? {
                if entry.metadata().await?.is_dir() {
                    stack.push(entry.path());
                }
            }
        }

        dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
        for dir in dirs {
            if dir == self.root {
                continue;
            }
            if is_dir_empty(&dir).await? {
                let _ = tokio::fs::remove_dir(&dir).await;
            }
        }
        Ok(())
    }

    async fn download_one(
        &self,
        channel_id: u64,
        attachment: &Attachment,
    ) -> anyhow::Result<UploadedFile> {
        let url = if !attachment.url.is_empty() {
            attachment.url.as_str()
        } else {
            attachment.proxy_url.as_str()
        };

        let resp = self.client.get(url).send().await?;
        if !resp.status().is_success() {
            anyhow::bail!("download failed with status {}", resp.status());
        }

        let bytes = resp.bytes().await?;
        if bytes.len() as u64 > self.max_file_bytes {
            anyhow::bail!("downloaded file too large: {} bytes", bytes.len());
        }

        let now = chrono::Utc::now();
        let channel_dir = self
            .root
            .join(channel_id.to_string())
            .join(now.format("%Y%m%d").to_string());
        tokio::fs::create_dir_all(&channel_dir).await?;

        let safe_name = sanitize_filename(&attachment.filename);
        let local_name = format!("{}-{}-{}", now.timestamp(), Uuid::new_v4(), safe_name);
        let local_path = channel_dir.join(local_name);

        tokio::fs::write(&local_path, &bytes).await?;

        Ok(UploadedFile {
            id: attachment.id.to_string(),
            name: attachment.filename.clone(),
            mime: attachment
                .content_type
                .clone()
                .unwrap_or_else(|| guess_mime_from_name(&attachment.filename)),
            size: bytes.len() as u64,
            local_path: local_path.to_string_lossy().to_string(),
            source_url: attachment.url.clone(),
        })
    }
}

async fn is_dir_empty(path: &Path) -> anyhow::Result<bool> {
    let mut rd = tokio::fs::read_dir(path).await?;
    Ok(rd.next_entry().await?.is_none())
}

fn sanitize_filename(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        let valid = c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-';
        out.push(if valid { c } else { '_' });
    }

    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "file.bin".to_string()
    } else {
        trimmed
    }
}

fn guess_mime_from_name(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".png") {
        return "image/png".to_string();
    }
    if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        return "image/jpeg".to_string();
    }
    if lower.ends_with(".gif") {
        return "image/gif".to_string();
    }
    if lower.ends_with(".webp") {
        return "image/webp".to_string();
    }
    if lower.ends_with(".pdf") {
        return "application/pdf".to_string();
    }
    "application/octet-stream".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::tempdir;

    fn test_manager(root: PathBuf, ttl: Duration, cleanup_interval: Duration) -> UploadManager {
        UploadManager {
            client: reqwest::Client::new(),
            root,
            max_file_bytes: 1024 * 1024,
            ttl,
            cleanup_interval,
            last_cleanup: Mutex::new(None),
        }
    }

    #[test]
    fn test_sanitize_filename_rewrites_invalid_chars() {
        assert_eq!(sanitize_filename("..//測試?.png"), ".._____.png");
        assert_eq!(sanitize_filename("!!!"), "file.bin");
        assert_eq!(sanitize_filename("hello-world.txt"), "hello-world.txt");
    }

    #[test]
    fn test_guess_mime_from_name_variants() {
        assert_eq!(guess_mime_from_name("a.PNG"), "image/png");
        assert_eq!(guess_mime_from_name("a.jpeg"), "image/jpeg");
        assert_eq!(guess_mime_from_name("a.gif"), "image/gif");
        assert_eq!(guess_mime_from_name("a.webp"), "image/webp");
        assert_eq!(guess_mime_from_name("a.pdf"), "application/pdf");
        assert_eq!(
            guess_mime_from_name("unknown.bin"),
            "application/octet-stream"
        );
    }

    #[tokio::test]
    async fn test_cleanup_expired_removes_old_files_and_empty_dirs() {
        let dir = tempdir().expect("tempdir");
        let nested = dir.path().join("chan").join("date");
        tokio::fs::create_dir_all(&nested).await.expect("mkdir");
        tokio::fs::write(nested.join("old.txt"), "x")
            .await
            .expect("write");

        let manager = test_manager(
            dir.path().to_path_buf(),
            Duration::from_secs(0),
            Duration::from_secs(0),
        );
        manager.cleanup_expired().await.expect("cleanup");

        assert!(is_dir_empty(dir.path()).await.expect("dir check"));
    }

    #[tokio::test]
    async fn test_maybe_cleanup_respects_interval() {
        let dir = tempdir().expect("tempdir");
        let manager = test_manager(
            dir.path().to_path_buf(),
            Duration::from_secs(0),
            Duration::from_secs(3600),
        );

        manager.maybe_cleanup().await;
        let first = *manager.last_cleanup.lock().await;
        assert!(first.is_some());
        manager.maybe_cleanup().await;
        let second = *manager.last_cleanup.lock().await;
        assert_eq!(first, second);
    }
}
