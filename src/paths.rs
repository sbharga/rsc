use anyhow::{Context, Result};
use std::fs::{self, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub fn root() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".rsc")
}

pub fn accounts_dir() -> PathBuf {
    root().join("accounts")
}

pub fn research_dir() -> PathBuf {
    root().join("research")
}

/// Mode 0700 on Unix. On Windows the directory inherits the parent's ACL (the user profile).
fn ensure_dir(p: &Path) -> Result<()> {
    if !p.exists() {
        let mut b = fs::DirBuilder::new();
        b.recursive(true);
        #[cfg(unix)]
        b.mode(0o700);
        b.create(p).with_context(|| format!("create {}", p.display()))?;
    }
    #[cfg(unix)]
    {
        let mode = fs::metadata(p)?.permissions().mode() & 0o777;
        if mode != 0o700 {
            fs::set_permissions(p, fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

pub fn ensure_dirs() -> Result<()> {
    ensure_dir(&root())?;
    ensure_dir(&accounts_dir())?;
    ensure_dir(&research_dir())?;
    Ok(())
}

/// Write `bytes` to `path` (mode 0600 on Unix) via tmp file + rename.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    // Unique per call: concurrent writers to one path (parallel agents persisting) must not share a tmp file.
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.{}.tmp", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)));
    let tmp = PathBuf::from(tmp);
    {
        let mut opts = OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        opts.mode(0o600);
        let mut f = opts.open(&tmp).with_context(|| format!("open {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("rename to {}", path.display()));
    }
    Ok(())
}

/// Create a private directory (and parents): mode 0700 on Unix.
pub fn create_private_dir(p: &Path) -> Result<()> {
    ensure_dir(p)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_atomic_writes_to_one_path_all_succeed() {
        let dir = std::env::temp_dir().join(format!("rsc-paths-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("research.json");
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let target = target.clone();
                std::thread::spawn(move || {
                    for i in 0..50 {
                        atomic_write(&target, format!("{t}-{i}").as_bytes()).unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert!(fs::read_to_string(&target).unwrap().contains('-'));
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1, "tmp files left behind");
        fs::remove_dir_all(&dir).unwrap();
    }
}
