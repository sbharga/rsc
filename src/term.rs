//! Colored CLI output. Color only when the stream is a terminal, NO_COLOR is unset and TERM isn't dumb. No ratatui.

use std::fmt::Display;
use std::io::IsTerminal;
use std::sync::LazyLock;

#[derive(Clone, Copy)]
pub struct Paint(bool);

/// Painter for stdout.
pub fn out() -> Paint {
    static ON: LazyLock<bool> = LazyLock::new(|| enabled(std::io::stdout().is_terminal()));
    Paint(*ON)
}

/// Painter for stderr.
pub fn err() -> Paint {
    static ON: LazyLock<bool> = LazyLock::new(|| enabled(std::io::stderr().is_terminal()));
    Paint(*ON)
}

fn enabled(tty: bool) -> bool {
    tty && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty()) && ansi()
}

/// Enables virtual terminal processing on the console.
#[cfg(windows)]
fn ansi() -> bool {
    crossterm::ansi_support::supports_ansi()
}

#[cfg(not(windows))]
fn ansi() -> bool {
    std::env::var_os("TERM").is_none_or(|t| t != "dumb")
}

impl Paint {
    fn sgr(self, code: &str, s: impl Display) -> String {
        if self.0 { format!("\x1b[{code}m{s}\x1b[0m") } else { s.to_string() }
    }

    pub fn bold(self, s: impl Display) -> String {
        self.sgr("1", s)
    }

    pub fn dim(self, s: impl Display) -> String {
        self.sgr("2", s)
    }

    pub fn red(self, s: impl Display) -> String {
        self.sgr("31", s)
    }

    pub fn green(self, s: impl Display) -> String {
        self.sgr("32", s)
    }

    pub fn yellow(self, s: impl Display) -> String {
        self.sgr("33", s)
    }

    pub fn cyan(self, s: impl Display) -> String {
        self.sgr("36", s)
    }
}

/// `rsc: msg` on stderr (prefix dimmed). Progress and info lines.
pub fn note(msg: impl Display) {
    eprintln!("{} {msg}", err().dim("rsc:"));
}

/// `error: msg` on stderr ("error:" bold red).
pub fn error(msg: impl Display) {
    let p = err();
    eprintln!("{} {msg}", p.red(p.bold("error:")));
}

/// `warning: msg` on stderr ("warning:" bold yellow).
pub fn warn(msg: impl Display) {
    let p = err();
    eprintln!("{} {msg}", p.yellow(p.bold("warning:")));
}

/// `error: {e}` then one `  caused by: {cause}` line per underlying cause.
pub fn report(e: &anyhow::Error) {
    error(e);
    let p = err();
    for cause in e.chain().skip(1) {
        eprintln!("  {} {cause}", p.dim("caused by:"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paint_wraps_only_when_enabled() {
        assert_eq!(Paint(true).bold("x"), "\x1b[1mx\x1b[0m");
        assert_eq!(Paint(false).bold("x"), "x");
    }
}
