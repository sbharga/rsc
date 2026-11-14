use crate::{auth, cli::UsageError, paths, term};
use anyhow::{Context, bail};
use std::io::{IsTerminal, Write};
use std::path::Path;

#[derive(Debug, Default, PartialEq)]
struct Opts {
    /// `Some(true)` = `--purge`, `Some(false)` = `--keep-data`, `None` = ask.
    purge: Option<bool>,
    yes: bool,
}

fn parse(args: &[String]) -> anyhow::Result<Opts> {
    let mut o = Opts::default();
    for a in args {
        let choice = match a.as_str() {
            "--purge" => Some(true),
            "--keep-data" => Some(false),
            "--yes" | "-y" => {
                o.yes = true;
                continue;
            }
            x if x.starts_with('-') => bail!("Unknown option {x}."),
            _ => return Err(UsageError.into()),
        };
        if o.purge.is_some_and(|p| Some(p) != choice) {
            bail!("Pass --purge or --keep-data, not both.");
        }
        o.purge = choice;
    }
    Ok(o)
}

/// Ask a yes/no question on stderr; default (and EOF or a read error) is No.
fn confirm(question: &str) -> bool {
    let p = term::err();
    eprint!("{} {} ", p.bold(question), p.dim("[y/N]"));
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
}

/// `rsc uninstall`: remove the binary and, if asked, sign out and delete `~/.rsc`.
pub async fn run(args: &[String]) -> anyhow::Result<i32> {
    let opts = parse(args)?;
    let exe = std::env::current_exe().context("Couldn't locate the rsc binary.")?;
    let root = paths::root();
    let has_data = root.exists();
    let tty = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    if !opts.yes && !tty {
        bail!("Pass --yes to uninstall when not running in a terminal.");
    }
    if !opts.yes && !confirm(&format!("Remove rsc from {}?", exe.display())) {
        term::note("Uninstall cancelled.");
        return Ok(130);
    }
    let purge = match opts.purge {
        Some(v) => v,
        None if has_data && !opts.yes => {
            confirm(&format!("Also delete your data in {} (sign-in, settings, research history)?", root.display()))
        }
        None => false,
    };

    let p = term::err();
    remove_binary(&exe)?;
    if cfg!(windows) {
        eprintln!("{} {} after rsc exits.", p.green("Removing"), exe.display());
    } else {
        eprintln!("{} {}", p.green("Removed"), exe.display());
    }

    if purge && has_data {
        // Revoke refresh tokens first: deleting the files alone would leave them valid.
        for a in auth::list_accounts().into_iter().filter(auth::Account::has_tokens) {
            if !auth::sign_out(&a.id()).await {
                term::warn(format!(
                    "Couldn't confirm sign-out for {}; disconnect rsc in ChatGPT settings if needed.",
                    a.email
                ));
            }
        }
        std::fs::remove_dir_all(&root).with_context(|| format!("Couldn't delete {}.", root.display()))?;
        eprintln!("{} {}", p.green("Deleted"), root.display());
    } else if has_data {
        term::note(format!("Kept your data in {}.", root.display()));
    }
    eprintln!("{}", p.green(p.bold("rsc uninstalled.")));
    Ok(0)
}

/// Unlinking a running binary is fine on Unix.
#[cfg(unix)]
fn remove_binary(exe: &Path) -> anyhow::Result<()> {
    std::fs::remove_file(exe).with_context(|| format!("Couldn't remove {}.", exe.display()))
}

/// A running .exe can't be deleted: a detached `cmd` deletes it once rsc has exited.
#[cfg(windows)]
fn remove_binary(exe: &Path) -> anyhow::Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut script = format!("/C ping -n 3 127.0.0.1 >NUL & del /F /Q \"{0}\" \"{0}.old\"", exe.display());
    // The install.ps1 default directory; `rmdir` without /S only removes it when empty.
    let default_dir = dirs::data_local_dir().map(|d| d.join("Programs").join("rsc"));
    if let Some(dir) = exe.parent().filter(|d| default_dir.as_deref() == Some(*d)) {
        script.push_str(&format!(" & rmdir \"{}\"", dir.display()));
    }
    Command::new("cmd.exe")
        .raw_arg(script)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .with_context(|| format!("Couldn't remove {}.", exe.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn accepts_flags() {
        assert_eq!(parse(&args(&["--purge", "--yes"])).unwrap(), Opts { purge: Some(true), yes: true });
        assert_eq!(parse(&args(&["--keep-data"])).unwrap(), Opts { purge: Some(false), yes: false });
        assert_eq!(parse(&args(&["-y"])).unwrap(), Opts { purge: None, yes: true });
        assert_eq!(parse(&args(&["--purge", "--purge"])).unwrap().purge, Some(true));
    }

    #[test]
    fn rejects_bad_input() {
        let e = parse(&args(&["--purge", "--keep-data"])).unwrap_err();
        assert_eq!(e.to_string(), "Pass --purge or --keep-data, not both.");
        let e = parse(&args(&["--bogus"])).unwrap_err();
        assert_eq!(e.to_string(), "Unknown option --bogus.");
        assert!(parse(&args(&["extra"])).unwrap_err().is::<UsageError>());
    }
}
