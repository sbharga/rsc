use crate::api::ApiError;
use crate::config::StageModel;
use crate::paths;
use crate::pricing::{Pricing, Usage};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Planning,
    Clarifying,
    Review,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentRec {
    pub title: String,
    pub brief: String,
    pub status: AgentStatus,
    pub error: Option<String>,
    pub usage: Usage,
    pub cost_usd: Option<f64>,
    pub turns: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SynthRec {
    pub usage: Usage,
    pub cost_usd: Option<f64>,
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
    pub agents: u8,
    pub max_parallel: u8,
    pub fetch_url: bool,
    pub incognito: bool,
    pub report_words: u32,
    pub max_turns: u32,
    pub compact_threshold_tokens: u64,
    pub synth_input_budget_tokens: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Clarification {
    pub question: String,
    pub answer: String,
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
        }
    }

    pub fn dir(&self) -> PathBuf {
        paths::research_dir().join(&self.id)
    }

    pub fn total_usage(&self) -> Usage {
        let mut u = self.planner_usage;
        u += self.compaction_usage;
        u += self.synthesis.usage;
        for a in &self.agents {
            u += a.usage;
        }
        u
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
        for a in &mut self.agents {
            a.cost_usd = p.cost(&self.config.researcher.model, &a.usage);
            add(a.cost_usd, &a.usage);
        }
        self.synthesis.cost_usd = p.cost(&self.config.synthesizer.model, &self.synthesis.usage);
        add(self.synthesis.cost_usd, &self.synthesis.usage);
        self.cost_usd_total = total;
        self.cost_partial = partial;
    }

    /// Persist research.json unless incognito.
    pub fn save(&self) -> anyhow::Result<()> {
        if self.config.incognito {
            return Ok(());
        }
        let d = self.dir();
        paths::create_private_dir(&d)?;
        paths::atomic_write(&d.join("research.json"), &serde_json::to_vec_pretty(self)?)
    }

    pub fn write_agent_report(&self, idx: usize, text: &str) -> anyhow::Result<()> {
        if self.config.incognito {
            return Ok(());
        }
        let d = self.dir().join("agents");
        paths::create_private_dir(&d)?;
        paths::atomic_write(&d.join(format!("{}.md", idx + 1)), text.as_bytes())
    }

    pub fn write_final(&self, text: &str) -> anyhow::Result<()> {
        if self.config.incognito {
            return Ok(());
        }
        paths::create_private_dir(&self.dir())?;
        paths::atomic_write(&self.dir().join("report.md"), text.as_bytes())
    }

    pub fn read_agent_report(&self, idx: usize) -> Option<String> {
        std::fs::read_to_string(self.dir().join("agents").join(format!("{}.md", idx + 1))).ok()
    }

    pub fn read_final(&self) -> Option<String> {
        std::fs::read_to_string(self.dir().join("report.md")).ok()
    }
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

pub fn delete(id: &str) -> anyhow::Result<()> {
    // id comes from our own listing, but never let it escape the research dir
    anyhow::ensure!(!id.is_empty() && !id.contains(['/', '\\']) && id != "." && id != "..", "bad id");
    std::fs::remove_dir_all(paths::research_dir().join(id))?;
    Ok(())
}

pub fn progress(r: &Record, synth_chars: usize) -> f64 {
    match r.status {
        RunStatus::Planning => 0.02,
        RunStatus::Clarifying => 0.03,
        RunStatus::Review => 0.05,
        RunStatus::Researching | RunStatus::Failed | RunStatus::Cancelled | RunStatus::Interrupted => {
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
            incognito: true,
            report_words: 1,
            max_turns: 1,
            compact_threshold_tokens: 1,
            synth_input_budget_tokens: 1,
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
        for k in ["understanding", "planner_notes", "clarifications", "questions", "clarify_rounds"] {
            v.as_object_mut().unwrap().remove(k);
        }
        let r: Record = serde_json::from_value(v).unwrap();
        assert!(r.understanding.is_empty());
        assert_eq!(r.clarify_rounds, 0);
    }
}
