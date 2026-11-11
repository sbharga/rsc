use anyhow::{Context, Result};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub fn root() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".rsc")
}

pub fn accounts_dir() -> PathBuf {
    root().join("accounts")
}

pub fn research_dir() -> PathBuf {
    root().join("research")
}

fn ensure_dir(p: &Path) -> Result<()> {
    if !p.exists() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(p)
            .with_context(|| format!("create {}", p.display()))?;
    }
    let mode = fs::metadata(p)?.permissions().mode() & 0o777;
    if mode != 0o700 {
        fs::set_permissions(p, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub fn ensure_dirs() -> Result<()> {
    ensure_dir(&root())?;
    ensure_dir(&accounts_dir())?;
    ensure_dir(&research_dir())?;
    Ok(())
}

/// Write `bytes` to `path` with mode 0600 via tmp file + rename.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .with_context(|| format!("open {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(())
}

/// Create a 0700 directory (and parents).
pub fn create_private_dir(p: &Path) -> Result<()> {
    ensure_dir(p)
}
