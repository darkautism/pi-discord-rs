use std::fs;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

pub fn detect_home_dir() -> Option<String> {
    if let Ok(home) = std::env::var("HOME") {
        if !home.trim().is_empty() {
            return Some(home);
        }
    }
    dirs::home_dir().map(|p| p.to_string_lossy().to_string())
}

pub fn collect_candidate_bin_dirs() -> Vec<String> {
    let mut dirs = Vec::new();

    let home = detect_home_dir();
    if let Some(home) = home.as_deref() {
        dirs.push(format!("{}/.npm-global/bin", home));
        dirs.push(format!("{}/.opencode/bin", home));
        dirs.push(format!("{}/.local/bin", home));
        dirs.push(format!("{}/.volta/bin", home));
    }

    if let Ok(nvm_bin) = std::env::var("NVM_BIN") {
        dirs.push(nvm_bin);
    }

    if let Some(home) = home {
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
    }

    dirs.push("/usr/local/bin".to_string());
    dirs.push("/usr/bin".to_string());
    dirs.push("/snap/bin".to_string());

    // keep first occurrence order
    let mut deduped = Vec::new();
    for d in dirs {
        if !deduped.contains(&d) {
            deduped.push(d);
        }
    }
    deduped
}

#[cfg(test)]
fn contains_in_order(v: &[String], a: &str, b: &str) -> bool {
    let ai = v.iter().position(|x| x == a);
    let bi = v.iter().position(|x| x == b);
    matches!((ai, bi), (Some(i), Some(j)) if i < j)
}

#[cfg(test)]
mod order_tests {
    use super::{collect_candidate_bin_dirs, contains_in_order};

    #[test]
    fn test_user_bin_dirs_precede_system_dirs_when_present() {
        let dirs = collect_candidate_bin_dirs();
        // if user dir exists in candidate list, it should be searched before /usr/bin
        let user_dir = dirs
            .iter()
            .find(|d| d.ends_with("/.npm-global/bin"))
            .cloned();
        if let Some(user_dir) = user_dir {
            assert!(contains_in_order(&dirs, &user_dir, "/usr/bin"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{build_augmented_path, is_candidate_runnable, resolve_binary_path};
    use std::fs;
    use std::io::Write;

    #[cfg(unix)]
    fn make_executable(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).expect("set perms");
    }

    #[test]
    fn test_build_augmented_path_contains_original() {
        let base = "/bin:/usr/bin";
        let out = build_augmented_path(base);
        assert!(out.contains(base));
    }

    #[test]
    fn test_resolve_binary_path_falls_back_to_input() {
        let out = resolve_binary_path("definitely-not-existing-binary-xyz");
        assert_eq!(out, "definitely-not-existing-binary-xyz");
    }

    #[cfg(unix)]
    #[test]
    fn test_is_candidate_runnable_detects_broken_shebang() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join("bad-script");
        let mut f = fs::File::create(&file_path).expect("create file");
        writeln!(f, "#!/no/such/interpreter").expect("write");
        writeln!(f, "echo hi").expect("write");
        make_executable(&file_path);
        assert!(!is_candidate_runnable(&file_path));
    }
}
pub fn is_candidate_runnable(path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        if meta.permissions().mode() & 0o111 == 0 {
            return false;
        }
    }

    // Detect broken shebang interpreters (common ENOENT cause for npm shims).
    let mut file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return true,
    };
    let mut buf = [0_u8; 256];
    let n = match file.read(&mut buf) {
        Ok(n) => n,
        Err(_) => return true,
    };
    let head = String::from_utf8_lossy(&buf[..n]);
    if let Some(line) = head.lines().next() {
        if let Some(shebang) = line.strip_prefix("#!") {
            let mut parts = shebang.split_whitespace();
            if let Some(interpreter) = parts.next() {
                let interpreter = interpreter.trim();
                if interpreter.starts_with('/') && !Path::new(interpreter).exists() {
                    return false;
                }
            }
        }
    }

    true
}

pub fn resolve_binary_path(bin: &str) -> String {
    if Path::new(bin).exists() {
        return bin.to_string();
    }

    for dir in collect_candidate_bin_dirs() {
        let candidate = Path::new(&dir).join(bin);
        if is_candidate_runnable(&candidate) {
            return candidate.to_string_lossy().to_string();
        }
    }

    bin.to_string()
}

pub fn resolve_binary_with_env(env_key: &str, bin: &str) -> String {
    std::env::var(env_key)
        .ok()
        .filter(|v| is_candidate_runnable(Path::new(v)))
        .unwrap_or_else(|| resolve_binary_path(bin))
}

pub fn build_augmented_path(current_path: &str) -> String {
    let mut all = collect_candidate_bin_dirs();
    all.push(current_path.to_string());
    all.join(":")
}
