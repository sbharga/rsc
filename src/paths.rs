use anyhow::{Context, Result};
use std::fs::{self, OpenOptions};
use std::io::Write;
#[cfg(unix)]
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
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
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
    fs::rename(&tmp, path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(())
}

/// Create a private directory (and parents): mode 0700 on Unix.
pub fn create_private_dir(p: &Path) -> Result<()> {
    ensure_dir(p)
}
