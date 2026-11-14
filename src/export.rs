//! Export a finished report as Markdown, HTML, JSON or PDF. No ratatui.

use crate::research::store::{LinkStatus, Record};
use anyhow::{Context, bail};
use pulldown_cmark::{CowStr, Event, Options, Parser, Tag};
use serde_json::json;
use std::fmt::Write as _;
use std::path::Path;
use tokio::io::AsyncWriteExt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Md,
    Html,
    Json,
    Pdf,
}

impl Format {
    pub fn parse(s: &str) -> Option<Format> {
        match s {
            "md" | "markdown" => Some(Format::Md),
            "html" => Some(Format::Html),
            "json" => Some(Format::Json),
            "pdf" => Some(Format::Pdf),
            _ => None,
        }
    }

    /// By lowercase file extension.
    pub fn from_path(p: &Path) -> Option<Format> {
        let ext = p.extension()?.to_str()?.to_lowercase();
        match ext.as_str() {
            "htm" => Some(Format::Html),
            e => Format::parse(e),
        }
    }
}

/// A string as a YAML double-quoted scalar (JSON strings are valid YAML).
fn q(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

fn link_label(s: LinkStatus) -> &'static str {
    match s {
        LinkStatus::Live => "live",
        LinkStatus::Dead => "dead",
        LinkStatus::Unknown => "unknown",
        LinkStatus::Unchecked => "unchecked",
    }
}

/// YAML metadata block between `---` lines.
pub fn front_matter(rec: &Record) -> String {
    let c = &rec.config;
    let mut s = String::from("---\n");
    let _ = writeln!(s, "title: {}", q(&rec.question));
    let _ = writeln!(s, "id: {}", q(&rec.id));
    let _ = writeln!(s, "date: {}", q(&rec.created_at));
    let _ = writeln!(s, "status: {}", q(rec.status.label()));
    let _ = writeln!(s, "revision: {}", rec.revision.max(1));
    if !c.preset.is_empty() {
        let _ = writeln!(s, "preset: {}", q(&c.preset));
    }
    s.push_str("models:\n");
    for (k, m) in [
        ("planner", &c.planner),
        ("researcher", &c.researcher),
        ("compactor", &c.compactor),
        ("synthesizer", &c.synthesizer),
    ] {
        let _ = writeln!(s, "  {k}: {}", q(&format!("{} ({})", m.model, m.effort)));
    }
    let _ = writeln!(s, "agents: {}", rec.agents.len());
    let _ = writeln!(s, "tokens: {}", rec.total_usage().tokens());
    let _ = writeln!(s, "cost_usd: {:.4}", rec.cost_usd_total);
    let _ = writeln!(s, "cost_partial: {}", rec.cost_partial);
    if !rec.context.is_empty() {
        s.push_str("context:\n");
        for d in &rec.context {
            let _ = writeln!(s, "  - {}", q(&d.name));
        }
    }
    let cited: Vec<_> = rec.sources.iter().enumerate().filter(|(_, x)| x.in_final).collect();
    if !cited.is_empty() {
        s.push_str("sources:\n");
        for (i, x) in cited {
            let _ = writeln!(s, "  - n: {}", i + 1);
            let _ = writeln!(s, "    title: {}", q(&x.title));
            let _ = writeln!(s, "    url: {}", q(&x.url));
            let _ = writeln!(s, "    status: {}", link_label(x.status));
        }
    }
    s.push_str("---\n");
    s
}

pub fn markdown(rec: &Record, report: &str) -> String {
    format!("{}\n{report}", front_matter(rec))
}

const CSS: &str = "body{font-family:system-ui,sans-serif;max-width:46rem;margin:2rem auto;padding:0 1rem;line-height:1.6}\
pre,code{font-family:ui-monospace,monospace}\
pre{background:#f4f4f4;padding:.75rem;overflow-x:auto}\
code{background:#f4f4f4;padding:0 .2em}pre code{padding:0;background:none}\
table{border-collapse:collapse}th,td{border:1px solid #ccc;padding:.3rem .6rem}\
blockquote{border-left:4px solid #ccc;margin-left:0;padding-left:1rem;color:#555}\
.meta{color:#666;font-size:.85rem}\
@media (prefers-color-scheme:dark){body{background:#161616;color:#ddd}pre,code{background:#262626}\
th,td{border-color:#444}blockquote{border-color:#444;color:#aaa}.meta{color:#999}a{color:#7ab7ff}}";

pub fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            c => o.push(c),
        }
    }
    o
}

/// Links and images may only point at http(s), mailto or relative/fragment targets.
fn safe_url(u: &str) -> bool {
    let u = u.trim();
    match u.split_once(':') {
        Some((scheme, _)) if !scheme.contains(['/', '?', '#']) => {
            matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https" | "mailto")
        }
        _ => true,
    }
}

/// Report body as HTML. Model output is web-derived, so raw HTML is shown as text.
fn body_html(report: &str) -> String {
    let opts =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_FOOTNOTES | Options::ENABLE_TASKLISTS;
    let events = Parser::new_ext(report, opts).map(|e| match e {
        Event::Html(s) | Event::InlineHtml(s) => Event::Text(s),
        Event::Start(Tag::Link { link_type, dest_url, title, id }) if !safe_url(&dest_url) => {
            Event::Start(Tag::Link { link_type, dest_url: CowStr::Borrowed("#"), title, id })
        }
        Event::Start(Tag::Image { link_type, dest_url, title, id }) if !safe_url(&dest_url) => {
            Event::Start(Tag::Image { link_type, dest_url: CowStr::Borrowed("#"), title, id })
        }
        e => e,
    });
    let mut out = String::new();
    pulldown_cmark::html::push_html(&mut out, events);
    out
}

/// Self-contained page: inline CSS, no scripts or external resources.
pub fn html(rec: &Record, report: &str) -> String {
    let date = chrono::DateTime::parse_from_rfc3339(&rec.created_at)
        .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| rec.created_at.clone());
    let n = rec.agents.len();
    let meta = format!(
        "{date} · {n} agent{} · ${:.2} · rsc {}",
        if n == 1 { "" } else { "s" },
        rec.cost_usd_total,
        env!("CARGO_PKG_VERSION")
    );
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>{title}</title>\n\
<style>{CSS}</style>\n</head>\n<body>\n<header><h1>{title}</h1><p class=\"meta\">{meta}</p></header>\n\
<article>\n{body}</article>\n</body>\n</html>\n",
        title = esc(&rec.question),
        meta = esc(&meta),
        body = body_html(report),
    )
}

pub fn json(rec: &Record, report: &str) -> String {
    serde_json::to_string_pretty(&json!({"record": rec, "report": report})).unwrap_or_default()
}

/// Text for a format; PDF returns the Markdown that is handed to pandoc.
pub fn render(rec: &Record, report: &str, f: Format) -> String {
    match f {
        Format::Md | Format::Pdf => markdown(rec, report),
        Format::Html => html(rec, report),
        Format::Json => json(rec, report),
    }
}

/// Convert with pandoc, which writes `out` itself (not through `paths::atomic_write`).
pub async fn pdf(rec: &Record, report: &str, out: &Path) -> anyhow::Result<()> {
    let mut child = match tokio::process::Command::new("pandoc")
        .args(["--from", "markdown", "--standalone", "-o"])
        .arg(out)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!("PDF export needs pandoc on PATH."),
        Err(e) => return Err(e).context("Couldn't start pandoc."),
    };
    let mut stdin = child.stdin.take().context("Couldn't start pandoc.")?;
    let md = markdown(rec, report);
    // Write while pandoc runs: it may exit early on bad input and close the pipe.
    let feed = async move {
        let _ = stdin.write_all(md.as_bytes()).await;
    };
    let (out, ()) = tokio::join!(child.wait_with_output(), feed);
    let out = out.context("Couldn't run pandoc.")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let line = err.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("unknown error");
        bail!("pandoc failed: {line}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::StageModel;
    use crate::research::store::{RunConfig, Source};

    fn rec(question: &str) -> Record {
        let sm = StageModel { model: "m".into(), effort: "low".into() };
        let cfg = RunConfig {
            planner: sm.clone(),
            researcher: sm.clone(),
            compactor: sm.clone(),
            synthesizer: sm,
            agents: 1,
            max_parallel: 1,
            fetch_url: false,
            report_words: 0,
            max_turns: 1,
            compact_threshold_tokens: 1,
            synth_input_budget_tokens: 1,
            report_style: String::new(),
            max_run_tokens: 0,
            max_run_cost_usd: 0.0,
            check_links: false,
            verify_citations: false,
            gap_round: false,
            preset: String::new(),
        };
        let mut r = Record::new(question.into(), cfg);
        let src = |url: &str, in_final| Source {
            url: url.into(),
            title: "T".into(),
            agents: vec![],
            in_final,
            status: LinkStatus::Live,
            model_supplied: false,
            verdicts: vec![],
        };
        r.sources = vec![src("https://a.test", false), src("https://b.test", true)];
        r
    }

    #[test]
    fn front_matter_quotes_and_numbers_cited_sources() {
        let fm = front_matter(&rec("a \"b\": c"));
        assert!(fm.starts_with("---\ntitle: \"a \\\"b\\\": c\"\n"), "{fm}");
        assert!(fm.contains("  - n: 2\n    title: \"T\"\n    url: \"https://b.test\"\n    status: live\n"));
        assert!(!fm.contains("a.test"));
        assert!(fm.ends_with("---\n"));
    }

    #[test]
    fn html_neutralises_markup_and_unsafe_links() {
        let h = html(
            &rec("q <x>"),
            "<script>alert(1)</script>\n\n[bad](javascript:alert(1)) [ok](https://ok.test) ![i](data:x)",
        );
        assert!(h.starts_with("<!doctype html>"));
        assert!(h.contains("&lt;script&gt;"));
        assert!(!h.contains("<script>"));
        assert!(!h.contains("javascript:"));
        assert!(h.contains("href=\"https://ok.test\""));
        assert!(!h.contains("data:x"));
        assert!(h.contains("<title>q &lt;x&gt;</title>"));
    }

    #[test]
    fn format_from_path() {
        let f = |p: &str| Format::from_path(Path::new(p));
        assert_eq!(f("a.MD"), Some(Format::Md));
        assert_eq!(f("a.htm"), Some(Format::Html));
        assert_eq!(f("a.json"), Some(Format::Json));
        assert_eq!(f("a.pdf"), Some(Format::Pdf));
        assert_eq!(f("a.txt"), None);
        assert_eq!(f("noext"), None);
    }
}
