use crate::paths;
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub planner: StageModel,
    pub researcher: StageModel,
    pub compactor: StageModel,
    pub synthesizer: StageModel,
    pub default_agents: u8,
    pub max_parallel: u8,
    /// Target report length per researcher; 0 lets the model size each report to the problem.
    pub report_words: u32,
    pub max_turns: u32,
    pub compact_threshold_tokens: u64,
    pub synth_input_budget_tokens: u64,
    pub fetch_url_enabled: bool,
    pub incognito_default: bool,
    /// Check GitHub releases for a newer rsc on launch.
    pub check_updates: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            planner: StageModel::new("medium"),
            researcher: StageModel::new("medium"),
            compactor: StageModel::new("low"),
            synthesizer: StageModel::new("high"),
            default_agents: 4,
            max_parallel: 3,
            report_words: 0,
            max_turns: 30,
            compact_threshold_tokens: 200_000,
            synth_input_budget_tokens: 120_000,
            fetch_url_enabled: false,
            incognito_default: false,
            check_updates: true,
        }
    }
}

impl Config {
    fn path() -> std::path::PathBuf {
        paths::root().join("config.toml")
    }

    pub fn clamp(&mut self) {
        self.default_agents = self.default_agents.clamp(1, MAX_AGENTS);
        self.max_parallel = self.max_parallel.clamp(1, MAX_AGENTS);
        if self.report_words != 0 {
            self.report_words = self.report_words.max(100);
        }
        self.max_turns = self.max_turns.max(1);
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
    }
}
