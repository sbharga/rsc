//! Built-in stage prompts and the user's editable copies in `~/.rsc/prompts/`.
//!
//! A run stores the prompts it started with, so editing a file never changes a run in progress or its resume.

use crate::paths;
use serde::{Deserialize, Serialize};

/// Placeholders: `{team}`, `{step3}`.
pub const PLANNER: &str = "You plan research for {team}. First make sure you understand exactly what the user wants, then give each researcher a distinct assignment.\n\n\
1. Do light research: at most 3 quick web searches to learn key terms, the current state of the topic and what matters for scoping. Do not research in depth — the researchers will.\n\
2. If the request is ambiguous in a way that would change what the researchers should do (scope, audience, timeframe, region, depth, constraints, or the kind of answer wanted), put up to 3 short, specific questions in `questions` and leave `perspectives` empty. Do not ask about anything you can reasonably assume.\n\
3. Otherwise leave `questions` empty and {step3}\n\n\
Always fill `understanding` with 1–3 sentences stating what the user is asking for, including any constraints from their answers, and `notes` with the key facts you found (with source URLs) that shaped the plan.";

/// Placeholders: `{title}`, `{brief}`, `{length}`. A note that web content is untrusted is always appended.
pub const RESEARCHER: &str = "You are one of several researchers working independently on the problem below. Your angle: {title} — {brief}\nInvestigate as you see fit using the available tools. When finished, reply with a Markdown report {length}: key findings, evidence, open uncertainties, and the sources you relied on as Markdown links [title](url).";

pub const SYNTHESIZER: &str = "Several researchers investigated the problem below from different angles. Combine their reports into one final answer in Markdown: integrate findings and resolve or flag conflicts. Cite sources by their number from the Sources list in square brackets, e.g. [3] or [2, 5]. If you use a source that is not on the list, cite it as a Markdown link [title](url). Do not write your own sources or references section; one is added automatically. You may search the web to verify key claims.";

pub const CHAT: &str = "You wrote the research report below for the user. Answer their follow-up questions about it in Markdown. Base answers on the report and its sources; search the web when a question needs information the report does not cover or to check a claim. Cite sources from the report's Sources list by number in square brackets, e.g. [3]; cite any new source as a Markdown link [title](url). Say so when the report and sources do not support an answer.";

const FILES: [(&str, &str); 4] =
    [("planner.md", PLANNER), ("researcher.md", RESEARCHER), ("synthesizer.md", SYNTHESIZER), ("chat.md", CHAT)];

/// The prompts a run uses. An empty field (a run from before prompts were stored) means the built-in.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prompts {
    pub planner: String,
    pub researcher: String,
    pub synthesizer: String,
    pub chat: String,
}

fn or_builtin<'a>(s: &'a str, builtin: &'a str) -> &'a str {
    if s.trim().is_empty() { builtin } else { s }
}

impl Prompts {
    pub fn planner(&self) -> &str {
        or_builtin(&self.planner, PLANNER)
    }

    pub fn researcher(&self) -> &str {
        or_builtin(&self.researcher, RESEARCHER)
    }

    pub fn synthesizer(&self) -> &str {
        or_builtin(&self.synthesizer, SYNTHESIZER)
    }

    pub fn chat(&self) -> &str {
        or_builtin(&self.chat, CHAT)
    }

    /// Read `~/.rsc/prompts/*.md`; a missing, unreadable or blank file means the built-in text.
    /// Always returns the full text so the record holds exactly what the run began with.
    pub fn load() -> Prompts {
        let dir = paths::root().join("prompts");
        let read = |name: &str, builtin: &str| match std::fs::read_to_string(dir.join(name)) {
            Ok(s) if !s.trim().is_empty() => s.trim_end().to_string(),
            _ => builtin.to_string(),
        };
        Prompts {
            planner: read("planner.md", PLANNER),
            researcher: read("researcher.md", RESEARCHER),
            synthesizer: read("synthesizer.md", SYNTHESIZER),
            chat: read("chat.md", CHAT),
        }
    }
}

/// Write the built-in prompts to `~/.rsc/prompts/` for the files that don't exist yet. Errors are ignored:
/// without the files the built-ins are used.
pub fn seed() {
    let dir = paths::root().join("prompts");
    if paths::create_private_dir(&dir).is_err() {
        return;
    }
    for (name, text) in FILES {
        let p = dir.join(name);
        if !p.exists() {
            let _ = paths::atomic_write(&p, format!("{text}\n").as_bytes());
        }
    }
}

/// Replace `{key}` for each known key in one pass: values are not rescanned and unknown `{x}` stay as written.
pub fn fill(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let value =
            rest.find('}').and_then(|end| vars.iter().find(|(k, _)| *k == &rest[1..end]).map(|(_, v)| (*v, end)));
        match value {
            Some((v, end)) => {
                out.push_str(v);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('{');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_is_single_pass_and_keeps_unknown_keys() {
        assert_eq!(fill("a {x} {y} {z}", &[("x", "{y}"), ("y", "2")]), "a {y} 2 {z}");
        assert_eq!(fill("no braces, a } and a {", &[("x", "1")]), "no braces, a } and a {");
    }

    #[test]
    fn empty_fields_fall_back_to_builtins() {
        let p = Prompts { planner: "mine".into(), ..Prompts::default() };
        assert_eq!((p.planner(), p.chat()), ("mine", CHAT));
    }
}
