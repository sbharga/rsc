use super::prompts::Prompts;
use crate::api::ApiError;
use crate::config::StageModel;
use crate::paths;
use crate::pricing::{Pricing, Usage};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Planning,
    Clarifying,
    Review,
    GapReview,
    Researching,
    Synthesizing,
    Done,
    Failed,
    Cancelled,
    Interrupted,
}

impl RunStatus {
    pub fn label(self) -> &'static str {
        match self {
            RunStatus::Planning => "planning",
            RunStatus::Clarifying => "clarifying",
            RunStatus::Review => "review",
            RunStatus::GapReview => "gap review",
            RunStatus::Researching => "researching",
            RunStatus::Synthesizing => "synthesizing",
            RunStatus::Done => "done",
            RunStatus::Failed => "failed",
            RunStatus::Cancelled => "cancelled",
            RunStatus::Interrupted => "interrupted",
        }
    }
    pub fn is_live(self) -> bool {
        matches!(self, RunStatus::Planning | RunStatus::Researching | RunStatus::Synthesizing)
    }
    pub fn is_resumable(self) -> bool {
        matches!(self, RunStatus::Failed | RunStatus::Cancelled | RunStatus::Interrupted)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Pending,
    Running,
    Done,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Perspective {
    pub title: String,
    pub brief: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkStatus {
    #[default]
    Unchecked,
    Live,
    Dead,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Support {
    Supported,
    Partial,
    Unsupported,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Verdict {
    pub claim: String,
    pub verdict: Support,
    pub note: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Source {
    pub url: String,
    #[serde(default)]
    pub title: String,
    /// 0-based indices of agents that cited or opened it, sorted, unique.
    #[serde(default)]
    pub agents: Vec<usize>,
    /// Cited in the final report.
    #[serde(default)]
    pub in_final: bool,
    #[serde(default)]
    pub status: LinkStatus,
    /// No stage opened it or got it as a search annotation; seen only as a link in model text.
    #[serde(default)]
    pub model_supplied: bool,
    #[serde(default)]
    pub verdicts: Vec<Verdict>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentRec {
    pub title: String,
    pub brief: String,
    pub status: AgentStatus,
    pub error: Option<String>,
    pub usage: Usage,
    pub cost_usd: Option<f64>,
    pub turns: u32,
    /// Added by the follow-up round.
    #[serde(default)]
    pub followup: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SynthRec {
    pub usage: Usage,
    pub cost_usd: Option<f64>,
    /// `[n]` numbers in the final text that were not on the Sources list given to the synthesizer.
    #[serde(default)]
    pub invalid_citations: Vec<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ErrRec {
    pub code: Option<String>,
    pub message: String,
    pub request_id: Option<String>,
}

impl From<&ApiError> for ErrRec {
    fn from(e: &ApiError) -> Self {
        ErrRec { code: e.code.clone(), message: e.message.clone(), request_id: e.request_id.clone() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunConfig {
    pub planner: StageModel,
    pub researcher: StageModel,
    pub compactor: StageModel,
    pub synthesizer: StageModel,
    /// `0` = the planner chooses (2–8).
    pub agents: u8,
    pub max_parallel: u8,
    pub fetch_url: bool,
    pub report_words: u32,
    pub max_turns: u32,
    pub compact_threshold_tokens: u64,
    pub synth_input_budget_tokens: u64,
    #[serde(default)]
    pub report_style: String,
    /// Stop starting researchers once a run has used this many tokens; 0 = no cap.
    #[serde(default)]
    pub max_run_tokens: u64,
    /// Same, in estimated USD; 0 = no cap.
    #[serde(default)]
    pub max_run_cost_usd: f64,
    #[serde(default)]
    pub check_links: bool,
    #[serde(default)]
    pub verify_citations: bool,
    #[serde(default)]
    pub gap_round: bool,
    /// Preset the run started from; empty = none.
    #[serde(default)]
    pub preset: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Clarification {
    pub question: String,
    pub answer: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    User,
    Assistant,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChatMsg {
    pub role: ChatRole,
    pub text: String,
    /// Research the assistant proposed in this answer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub research: Vec<Perspective>,
}

/// `research/<id>/chat.json`: follow-up conversation about the final report.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatLog {
    pub messages: Vec<ChatMsg>,
    pub usage: Usage,
}

/// A local file attached to a run; its text is part of the task brief.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LocalDoc {
    pub name: String,
    pub path: String,
    pub text: String,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub question: String,
    pub created_at: String,
    pub status: RunStatus,
    pub error: Option<ErrRec>,
    pub config: RunConfig,
    pub perspectives: Vec<Perspective>,
    pub agents: Vec<AgentRec>,
    pub synthesis: SynthRec,
    pub planner_usage: Usage,
    pub compaction_usage: Usage,
    pub cost_usd_total: f64,
    pub cost_partial: bool,
    #[serde(default)]
    pub understanding: String,
    #[serde(default)]
    pub planner_notes: String,
    #[serde(default)]
    pub clarifications: Vec<Clarification>,
    /// Pending questions that have not been answered.
    #[serde(default)]
    pub questions: Vec<String>,
    #[serde(default)]
    pub clarify_rounds: u8,
    #[serde(default)]
    pub sources: Vec<Source>,
    /// A budget cap stopped research early.
    #[serde(default)]
    pub budget_hit: bool,
    /// Proposed follow-up angles awaiting approval.
    #[serde(default)]
    pub gaps: Vec<Perspective>,
    /// The gap reviewer's list of gaps and contradictions.
    #[serde(default)]
    pub gap_notes: String,
    /// The gap reviewer has run for this run; never review again.
    #[serde(default)]
    pub gaps_reviewed: bool,
    #[serde(default)]
    pub verify_usage: Usage,
    /// Local files attached to the run.
    #[serde(default)]
    pub context: Vec<LocalDoc>,
    /// Report revision; 0 or 1 = original. Earlier revisions are in `report.v<n>.md`.
    #[serde(default)]
    pub revision: u32,
    /// Chat questions that started a deep-dive.
    #[serde(default)]
    pub followups: Vec<String>,
    /// Prompts the run started with; resume and chat use these, not the files in `~/.rsc/prompts`.
    #[serde(default)]
    pub prompts: Prompts,
}

pub fn new_id() -> String {
    let mut b = [0u8; 2];
    rand::fill(&mut b[..]);
    format!("{}-{:02x}{:02x}", chrono::Local::now().format("%Y%m%d-%H%M%S"), b[0], b[1])
}

impl Record {
    pub fn new(question: String, config: RunConfig) -> Record {
        Record {
            id: new_id(),
            question,
            created_at: chrono::Utc::now().to_rfc3339(),
            status: RunStatus::Planning,
            error: None,
            config,
            perspectives: vec![],
            agents: vec![],
            synthesis: SynthRec::default(),
            planner_usage: Usage::default(),
            compaction_usage: Usage::default(),
            cost_usd_total: 0.0,
            cost_partial: false,
            understanding: String::new(),
            planner_notes: String::new(),
            clarifications: vec![],
            questions: vec![],
            clarify_rounds: 0,
            sources: vec![],
            budget_hit: false,
            gaps: vec![],
            gap_notes: String::new(),
            gaps_reviewed: false,
            verify_usage: Usage::default(),
            context: vec![],
            revision: 0,
            followups: vec![],
            prompts: Prompts::default(),
        }
    }

    pub fn dir(&self) -> PathBuf {
        paths::research_dir().join(&self.id)
    }

    pub fn total_usage(&self) -> Usage {
        let mut u = self.planner_usage;
        u += self.compaction_usage;
        u += self.synthesis.usage;
        u += self.verify_usage;
        for a in &self.agents {
            u += a.usage;
        }
        u
    }

    /// Usage reached a configured cap. Call after `recompute_costs`.
    pub fn over_budget(&self) -> bool {
        let c = &self.config;
        (c.max_run_tokens > 0 && self.total_usage().tokens() >= c.max_run_tokens)
            || (c.max_run_cost_usd > 0.0 && self.cost_usd_total >= c.max_run_cost_usd)
    }

    /// Recompute per-part and total costs from usage.
    pub fn recompute_costs(&mut self, p: &Pricing) {
        let mut total = 0.0;
        let mut partial = false;
        let mut add = |c: Option<f64>, u: &Usage| match c {
            Some(c) => total += c,
            None => {
                partial = true;
                total += p.search_cost(u);
            }
        };
        add(p.cost(&self.config.planner.model, &self.planner_usage), &self.planner_usage);
        add(p.cost(&self.config.compactor.model, &self.compaction_usage), &self.compaction_usage);
        add(p.cost(&self.config.compactor.model, &self.verify_usage), &self.verify_usage);
        for a in &mut self.agents {
            a.cost_usd = p.cost(&self.config.researcher.model, &a.usage);
            add(a.cost_usd, &a.usage);
        }
        self.synthesis.cost_usd = p.cost(&self.config.synthesizer.model, &self.synthesis.usage);
        add(self.synthesis.cost_usd, &self.synthesis.usage);
        self.cost_usd_total = total;
        self.cost_partial = partial;
    }

    /// Persist research.json.
    pub fn save(&self) -> anyhow::Result<()> {
        let d = self.dir();
        paths::create_private_dir(&d)?;
        paths::atomic_write(&d.join("research.json"), &serde_json::to_vec_pretty(self)?)
    }

    pub fn write_agent_report(&self, idx: usize, text: &str) -> anyhow::Result<()> {
        let d = self.dir().join("agents");
        paths::create_private_dir(&d)?;
        paths::atomic_write(&d.join(format!("{}.md", idx + 1)), text.as_bytes())
    }

    pub fn write_final(&self, text: &str) -> anyhow::Result<()> {
        paths::create_private_dir(&self.dir())?;
        paths::atomic_write(&self.dir().join("report.md"), text.as_bytes())
    }

    pub fn read_agent_report(&self, idx: usize) -> Option<String> {
        std::fs::read_to_string(self.dir().join("agents").join(format!("{}.md", idx + 1))).ok()
    }

    pub fn read_final(&self) -> Option<String> {
        std::fs::read_to_string(self.dir().join("report.md")).ok()
    }

    /// Follow-up conversation; missing or unreadable file = empty.
    pub fn read_chat(&self) -> ChatLog {
        std::fs::read_to_string(self.dir().join("chat.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn write_chat(&self, log: &ChatLog) -> anyhow::Result<()> {
        paths::create_private_dir(&self.dir())?;
        paths::atomic_write(&self.dir().join("chat.json"), &serde_json::to_vec_pretty(log)?)
    }
}

/// `agents/<n>.state.json`: a researcher's input items after its last finished turn.
#[derive(Deserialize)]
pub struct AgentState {
    pub turns: u32,
    pub input: Vec<serde_json::Value>,
}

#[derive(Serialize)]
struct AgentStateOut<'a> {
    turns: u32,
    input: &'a [serde_json::Value],
}

fn agent_state_path(dir: &Path, idx: usize) -> PathBuf {
    dir.join("agents").join(format!("{}.state.json", idx + 1))
}

pub fn write_agent_state(dir: &Path, idx: usize, turns: u32, input: &[serde_json::Value]) -> anyhow::Result<()> {
    paths::create_private_dir(&dir.join("agents"))?;
    paths::atomic_write(&agent_state_path(dir, idx), &serde_json::to_vec(&AgentStateOut { turns, input })?)
}

/// Missing or unparsable = no checkpoint.
pub fn read_agent_state(dir: &Path, idx: usize) -> Option<AgentState> {
    serde_json::from_slice(&std::fs::read(agent_state_path(dir, idx)).ok()?).ok()
}

pub fn remove_agent_state(dir: &Path, idx: usize) {
    let _ = std::fs::remove_file(agent_state_path(dir, idx));
}

/// Load all persisted runs, newest first.
pub fn list() -> Vec<Record> {
    let mut v: Vec<Record> = std::fs::read_dir(paths::research_dir())
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| std::fs::read_to_string(e.path().join("research.json")).ok())
                .filter_map(|s| serde_json::from_str::<Record>(&s).ok())
                .collect()
        })
        .unwrap_or_default();
    v.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| b.id.cmp(&a.id)));
    v
}

/// Startup: live statuses left on disk mean the previous process died mid-run.
pub fn mark_interrupted() {
    for mut r in list() {
        if r.status.is_live() {
            r.status = RunStatus::Interrupted;
            for a in &mut r.agents {
                if a.status == AgentStatus::Running {
                    a.status = AgentStatus::Pending;
                }
            }
            let _ = r.save();
        }
    }
}

/// Never let an id escape the research dir.
fn check_id(id: &str) -> anyhow::Result<()> {
    anyhow::ensure!(!id.is_empty() && !id.contains(['/', '\\']) && id != "." && id != "..", "Invalid research id.");
    Ok(())
}

pub fn load(id: &str) -> anyhow::Result<Record> {
    check_id(id)?;
    let path = paths::research_dir().join(id).join("research.json");
    let s = std::fs::read_to_string(&path).with_context(|| format!("No research with id {id}."))?;
    Ok(serde_json::from_str(&s)?)
}

pub fn delete(id: &str) -> anyhow::Result<()> {
    check_id(id)?;
    std::fs::remove_dir_all(paths::research_dir().join(id))?;
    Ok(())
}

pub fn progress(r: &Record, synth_chars: usize) -> f64 {
    match r.status {
        RunStatus::Planning => 0.02,
        RunStatus::Clarifying => 0.03,
        RunStatus::Review => 0.05,
        RunStatus::GapReview
        | RunStatus::Researching
        | RunStatus::Failed
        | RunStatus::Cancelled
        | RunStatus::Interrupted => {
            if r.agents.is_empty() {
                return 0.05;
            }
            let sum: f64 = r
                .agents
                .iter()
                .map(|a| match a.status {
                    AgentStatus::Done | AgentStatus::Failed => 1.0,
                    AgentStatus::Running => (a.turns as f64 / 12.0).min(0.9),
                    AgentStatus::Pending => 0.0,
                })
                .sum();
            0.05 + 0.80 * (sum / r.agents.len() as f64)
        }
        RunStatus::Synthesizing => 0.85 + 0.15 * (synth_chars as f64 / 8000.0).min(0.95),
        RunStatus::Done => 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_state_round_trips_and_is_removable() {
        let dir = std::env::temp_dir().join(format!("rsc-test-{}", uuid::Uuid::new_v4()));
        assert!(read_agent_state(&dir, 1).is_none());
        let input = vec![serde_json::json!({"type":"message","role":"user"}), serde_json::json!({"k": [1, 2]})];
        write_agent_state(&dir, 1, 4, &input).unwrap();
        assert!(dir.join("agents/2.state.json").exists());
        let st = read_agent_state(&dir, 1).unwrap();
        assert_eq!((st.turns, st.input), (4, input));
        remove_agent_state(&dir, 1);
        assert!(read_agent_state(&dir, 1).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn rec(statuses: &[(AgentStatus, u32)]) -> Record {
        let sm = StageModel { model: "m".into(), effort: "low".into() };
        let cfg = RunConfig {
            planner: sm.clone(),
            researcher: sm.clone(),
            compactor: sm.clone(),
            synthesizer: sm,
            agents: 4,
            max_parallel: 3,
            fetch_url: false,
            report_words: 1,
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
        let mut r = Record::new("q".into(), cfg);
        r.status = RunStatus::Researching;
        r.agents = statuses
            .iter()
            .map(|(s, t)| AgentRec {
                title: "t".into(),
                brief: "b".into(),
                status: *s,
                error: None,
                usage: Usage::default(),
                cost_usd: None,
                turns: *t,
                followup: false,
            })
            .collect();
        r
    }

    #[test]
    fn progress_mixed() {
        let r = rec(&[
            (AgentStatus::Done, 0),
            (AgentStatus::Running, 6),
            (AgentStatus::Pending, 0),
            (AgentStatus::Failed, 0),
        ]);
        assert!((progress(&r, 0) - 0.55).abs() < 1e-9);
    }

    #[test]
    fn progress_synth_caps() {
        let mut r = rec(&[(AgentStatus::Done, 0)]);
        r.status = RunStatus::Synthesizing;
        assert!((progress(&r, 100_000) - (0.85 + 0.15 * 0.95)).abs() < 1e-9);
    }

    #[test]
    fn delete_rejects_traversal() {
        assert!(delete("../x").is_err());
        assert!(delete("").is_err());
    }

    #[test]
    fn record_without_planner_fields_loads() {
        let mut v = serde_json::to_value(rec(&[])).unwrap();
        for k in ["understanding", "planner_notes", "clarifications", "questions", "clarify_rounds", "sources"] {
            v.as_object_mut().unwrap().remove(k);
        }
        let r: Record = serde_json::from_value(v).unwrap();
        assert!(r.understanding.is_empty());
        assert_eq!(r.clarify_rounds, 0);
        assert!(r.sources.is_empty());
    }

    #[test]
    fn record_without_phase2_fields_loads() {
        let mut r = rec(&[(AgentStatus::Done, 0)]);
        r.sources.push(Source {
            url: "https://a.test/".into(),
            title: String::new(),
            agents: vec![0],
            in_final: true,
            status: LinkStatus::Live,
            model_supplied: true,
            verdicts: vec![Verdict { claim: "c".into(), verdict: Support::Partial, note: "n".into() }],
        });
        let mut v = serde_json::to_value(&r).unwrap();
        let o = v.as_object_mut().unwrap();
        for k in ["gaps", "gap_notes", "gaps_reviewed", "verify_usage"] {
            o.remove(k);
        }
        o["synthesis"].as_object_mut().unwrap().remove("invalid_citations");
        for k in ["check_links", "verify_citations", "gap_round"] {
            o["config"].as_object_mut().unwrap().remove(k);
        }
        o["agents"][0].as_object_mut().unwrap().remove("followup");
        for k in ["status", "model_supplied", "verdicts"] {
            o["sources"][0].as_object_mut().unwrap().remove(k);
        }
        let r: Record = serde_json::from_value(v).unwrap();
        assert!(r.gaps.is_empty() && r.gap_notes.is_empty() && !r.gaps_reviewed);
        assert_eq!(r.verify_usage.tokens(), 0);
        assert!(r.synthesis.invalid_citations.is_empty());
        assert!(!r.config.check_links && !r.config.verify_citations && !r.config.gap_round);
        assert!(!r.agents[0].followup);
        let s = &r.sources[0];
        assert_eq!(s.status, LinkStatus::Unchecked);
        assert!(!s.model_supplied && s.verdicts.is_empty());
    }

    #[test]
    fn over_budget() {
        let mut r = rec(&[(AgentStatus::Done, 0)]);
        r.agents[0].usage.input = 1_000_000;
        r.cost_usd_total = 100.0;
        assert!(!r.over_budget());
        r.config.max_run_tokens = 1000;
        r.agents[0].usage.input = 999;
        assert!(!r.over_budget());
        r.agents[0].usage.input = 1000;
        assert!(r.over_budget());
        r.config.max_run_tokens = 0;
        r.config.max_run_cost_usd = 1.0;
        r.cost_usd_total = 0.99;
        assert!(!r.over_budget());
        r.cost_usd_total = 1.0;
        assert!(r.over_budget());
    }

    #[test]
    fn record_without_budget_fields_loads() {
        let mut v = serde_json::to_value(rec(&[])).unwrap();
        v.as_object_mut().unwrap().remove("budget_hit");
        let cfg = v["config"].as_object_mut().unwrap();
        cfg.remove("max_run_tokens");
        cfg.remove("max_run_cost_usd");
        let r: Record = serde_json::from_value(v).unwrap();
        assert!(!r.budget_hit);
        assert_eq!((r.config.max_run_tokens, r.config.max_run_cost_usd), (0, 0.0));
    }

    #[test]
    fn chat_log_loads_partial_json() {
        let l: ChatLog = serde_json::from_str("{}").unwrap();
        assert!(l.messages.is_empty());
        assert_eq!(l.usage.tokens(), 0);
        let l: ChatLog = serde_json::from_str(r#"{"messages":[{"role":"assistant","text":"x"}]}"#).unwrap();
        assert_eq!(l.messages, vec![ChatMsg { role: ChatRole::Assistant, text: "x".into(), research: vec![] }]);
    }

    #[test]
    fn run_config_without_report_style_loads() {
        let mut v = serde_json::to_value(rec(&[])).unwrap();
        v["config"].as_object_mut().unwrap().remove("report_style");
        let r: Record = serde_json::from_value(v).unwrap();
        assert!(r.config.report_style.is_empty());
    }

    #[test]
    fn record_without_phase3_fields_loads() {
        let mut v = serde_json::to_value(rec(&[])).unwrap();
        let o = v.as_object_mut().unwrap();
        for k in ["context", "revision", "followups", "prompts"] {
            assert!(o.remove(k).is_some(), "{k}");
        }
        assert!(v["config"].as_object_mut().unwrap().remove("preset").is_some());
        let r: Record = serde_json::from_value(v).unwrap();
        assert!(r.context.is_empty() && r.followups.is_empty());
        assert_eq!((r.revision, r.config.preset.as_str()), (0, ""));
    }
}
