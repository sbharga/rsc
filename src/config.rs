use crate::paths;
use crate::research::store::RunConfig;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StageModel {
    pub model: String,
    pub effort: String,
}

impl StageModel {
    fn new(effort: &str) -> Self {
        Self { model: String::new(), effort: effort.into() }
    }
}

/// Upper bound for the number of agents and for how many run at once.
pub const MAX_AGENTS: u8 = 99;
/// Bounds on the number of researchers when the planner chooses.
pub const AUTO_MIN_AGENTS: u8 = 2;
pub const AUTO_MAX_AGENTS: u8 = 8;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub planner: StageModel,
    pub researcher: StageModel,
    pub compactor: StageModel,
    pub synthesizer: StageModel,
    /// Researchers per run; `0` lets the planner choose (2–8).
    pub default_agents: u8,
    pub max_parallel: u8,
    /// Target report length per researcher; 0 lets the model size each report to the problem.
    pub report_words: u32,
    /// Extra instructions for how the final report and chat answers are written; empty = none.
    pub report_style: String,
    pub max_turns: u32,
    pub compact_threshold_tokens: u64,
    pub synth_input_budget_tokens: u64,
    /// Stop starting researchers once a run has used this many tokens; 0 = no cap.
    pub max_run_tokens: u64,
    /// Same, in estimated USD; 0 = no cap.
    pub max_run_cost_usd: f64,
    pub fetch_url_enabled: bool,
    /// Check GitHub releases for a newer rsc on launch.
    pub check_updates: bool,
    /// Probe each cited URL (HEAD/GET) to flag dead links; sites see your IP.
    pub check_links: bool,
    /// Ask a model whether each cited page supports the claims that cite it.
    pub verify_citations: bool,
    /// Let a reviewer propose follow-up researchers after the first round.
    pub gap_round: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            planner: StageModel::new("medium"),
            researcher: StageModel::new("medium"),
            compactor: StageModel::new("low"),
            synthesizer: StageModel::new("high"),
            default_agents: 0,
            max_parallel: 3,
            report_words: 0,
            report_style: String::new(),
            max_turns: 30,
            compact_threshold_tokens: 200_000,
            synth_input_budget_tokens: 120_000,
            max_run_tokens: 0,
            max_run_cost_usd: 0.0,
            fetch_url_enabled: false,
            check_updates: true,
            check_links: true,
            verify_citations: false,
            gap_round: false,
        }
    }
}

impl Config {
    fn path() -> std::path::PathBuf {
        paths::root().join("config.toml")
    }

    pub fn clamp(&mut self) {
        if self.default_agents != 0 {
            self.default_agents = self.default_agents.clamp(1, MAX_AGENTS);
        }
        self.max_parallel = self.max_parallel.clamp(1, MAX_AGENTS);
        if self.report_words != 0 {
            self.report_words = self.report_words.max(100);
        }
        self.max_turns = self.max_turns.max(1);
        self.max_run_cost_usd = self.max_run_cost_usd.max(0.0);
    }

    /// Per-run settings: the given stage models and agent count, the rest copied from this config.
    pub fn run_config(
        &self,
        planner: StageModel,
        researcher: StageModel,
        compactor: StageModel,
        synthesizer: StageModel,
        agents: u8,
    ) -> RunConfig {
        RunConfig {
            planner,
            researcher,
            compactor,
            synthesizer,
            agents,
            max_parallel: self.max_parallel,
            fetch_url: self.fetch_url_enabled,
            report_words: self.report_words,
            max_turns: self.max_turns,
            compact_threshold_tokens: self.compact_threshold_tokens,
            synth_input_budget_tokens: self.synth_input_budget_tokens,
            report_style: self.report_style.trim().to_string(),
            max_run_tokens: self.max_run_tokens,
            max_run_cost_usd: self.max_run_cost_usd,
            check_links: self.check_links,
            verify_citations: self.verify_citations,
            gap_round: self.gap_round,
        }
    }

    /// Returns (config, optional error banner).
    pub fn load() -> (Config, Option<String>) {
        let p = Self::path();
        match std::fs::read_to_string(&p) {
            Ok(s) => match toml::from_str::<Config>(&s) {
                Ok(mut c) => {
                    c.clamp();
                    (c, None)
                }
                Err(e) => (Config::default(), Some(format!("config.toml invalid: {} — using defaults", e.message()))),
            },
            Err(_) => {
                let c = Config::default();
                let _ = c.save();
                (c, None)
            }
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        paths::atomic_write(&Self::path(), toml::to_string_pretty(self)?.as_bytes())
    }
}

/// Resolve a stage's model slug against the catalog (first model if empty/missing).
pub fn resolve_model(stage: &StageModel, catalog: &[String]) -> StageModel {
    let model = if !stage.model.is_empty() && catalog.iter().any(|m| m == &stage.model) {
        stage.model.clone()
    } else {
        catalog.first().cloned().unwrap_or_else(|| stage.model.clone())
    };
    StageModel { model, effort: stage.effort.clone() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_limits_agents_and_keeps_auto_length() {
        let mut c = Config { default_agents: 200, max_parallel: 0, report_words: 0, ..Config::default() };
        c.clamp();
        assert_eq!((c.default_agents, c.max_parallel, c.report_words), (MAX_AGENTS, 1, 0));
        c.report_words = 5;
        c.clamp();
        assert_eq!(c.report_words, 100);
        c.default_agents = 0;
        c.clamp();
        assert_eq!(c.default_agents, 0);
    }
}
