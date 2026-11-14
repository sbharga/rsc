use crate::research::store::Record;
use base64::Engine;
use std::io::Write;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn money(v: f64) -> String {
    format!("${v:.2}")
}

pub fn tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M tok", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{:.1}k tok", n as f64 / 1e3)
    } else {
        format!("{n} tok")
    }
}

pub fn badge(r: &Record) -> String {
    let plus = if r.cost_partial { "+" } else { "" };
    format!("{}{plus} · {}", money(r.cost_usd_total), tokens(r.total_usage().tokens()))
}

pub fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Truncate to a display width, adding `…` when cut.
pub fn trunc(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw > width - 1 {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

pub fn pad(s: &str, width: usize) -> String {
    let t = trunc(s, width);
    let w = t.width();
    format!("{t}{}", " ".repeat(width.saturating_sub(w)))
}

/// `m:ss`, or `h:mm:ss` from one hour.
pub fn elapsed(d: std::time::Duration) -> String {
    let s = d.as_secs();
    if s < 3600 {
        format!("{}:{:02}", s / 60, s % 60)
    } else {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    }
}

/// Greedy word wrap by display width.
pub fn wrap(s: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = vec![];
    let mut cur = String::new();
    for word in s.split_whitespace() {
        let need = if cur.is_empty() { word.width() } else { cur.width() + 1 + word.width() };
        if need > width && !cur.is_empty() {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

/// Copy to the terminal clipboard via OSC 52.
pub fn osc52_copy(text: &str) {
    let b = base64::engine::general_purpose::STANDARD.encode(text);
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b]52;c;{b}\x07");
    let _ = out.flush();
}

pub fn open_url(url: &str) {
    let _ = open::that_detached(url);
}

pub fn expand_tilde(p: &str) -> std::path::PathBuf {
    if let Some(rest) = p.strip_prefix("~/")
        && let Some(h) = dirs::home_dir()
    {
        return h.join(rest);
    }
    std::path::PathBuf::from(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(money(0.4199), "$0.42");
        assert_eq!(tokens(1_234_567), "1.2M tok");
        assert_eq!(tokens(999), "999 tok");
        assert_eq!(trunc("hello world", 6), "hello…");
        assert_eq!(trunc("hi", 6), "hi");
        assert_eq!(pad("ab", 4), "ab  ");
    }

    #[test]
    fn elapsed_formats() {
        let e = |s| elapsed(std::time::Duration::from_secs(s));
        assert_eq!(e(0), "0:00");
        assert_eq!(e(75), "1:15");
        assert_eq!(e(3725), "1:02:05");
    }
}
