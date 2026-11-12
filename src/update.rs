use anyhow::Context;
use serde::Deserialize;
use std::time::Duration;

const RELEASES_URL: &str = "https://api.github.com/repos/sbharga/rsc/releases?per_page=20";
const INSTALL_URL_BASE: &str = "https://raw.githubusercontent.com/sbharga/rsc";
const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    draft: bool,
}

/// Tag of the newest published release (pre-releases included) newer than this build.
pub async fn check() -> anyhow::Result<Option<String>> {
    let http = reqwest::Client::builder()
        .user_agent(concat!("rsc/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(10))
        .build()?;
    let releases: Vec<Release> = http
        .get(RELEASES_URL)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(newest(VERSION, releases.iter().filter(|r| !r.draft).map(|r| r.tag_name.as_str())))
}

/// `rsc update`: install the newest release over the running binary.
pub async fn run() -> anyhow::Result<()> {
    let Some(tag) = check().await.context("Couldn't check for updates.")? else {
        println!("rsc {VERSION} is up to date.");
        return Ok(());
    };
    let exe = std::env::current_exe().context("Couldn't locate the rsc binary.")?;
    let dir = exe.parent().context("Couldn't locate the rsc binary.")?;
    println!("Updating rsc {VERSION} to {}...", tag.trim_start_matches('v'));
    let url = format!("{INSTALL_URL_BASE}/{tag}/install.sh");
    let status = tokio::process::Command::new("sh")
        .args(["-c", r#"script=$(curl -fsSL "$1") && sh -c "$script""#, "sh", &url])
        .env("RSC_VERSION", &tag)
        .env("RSC_INSTALL_DIR", dir)
        .status()
        .await
        .context("Couldn't run the installer.")?;
    if !status.success() {
        anyhow::bail!("Update failed.");
    }
    Ok(())
}

/// Parse `[v]MAJOR.MINOR.PATCH[-pre][+build]` into a comparable triple; suffixes are ignored.
fn parse(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.strip_prefix('v').unwrap_or(v).split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let triple = (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?, parts.next()?.parse().ok()?);
    parts.next().is_none().then_some(triple)
}

/// The tag with the greatest version strictly above `current`, if any.
fn newest<'a>(current: &str, tags: impl Iterator<Item = &'a str>) -> Option<String> {
    let current = parse(current)?;
    tags.filter_map(|t| parse(t).map(|v| (v, t)))
        .filter(|(v, _)| *v > current)
        .max_by_key(|(v, _)| *v)
        .map(|(_, t)| t.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_picks_greatest_newer_tag() {
        let cases: [(&str, &[&str], Option<&str>); 5] = [
            ("0.0.1", &["v0.0.1"], None),
            ("0.0.1", &["v0.0.2", "v0.1.0", "v0.0.3"], Some("v0.1.0")),
            ("0.0.1", &["v0.0.0", "junk", "v1.2"], None),
            ("0.0.1", &["0.0.2"], Some("0.0.2")),
            ("0.0.2", &["v0.0.2-rc.1"], None),
        ];
        for (current, tags, want) in cases {
            assert_eq!(newest(current, tags.iter().copied()).as_deref(), want, "{current} {tags:?}");
        }
    }
}
