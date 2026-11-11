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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub planner: StageModel,
    pub researcher: StageModel,
    pub compactor: StageModel,
    pub synthesizer: StageModel,
    pub default_agents: u8,
    pub max_parallel: u8,
    pub report_words: u32,
    pub max_turns: u32,
    pub compact_threshold_tokens: u64,
    pub synth_input_budget_tokens: u64,
    pub fetch_url_enabled: bool,
    pub incognito_default: bool,
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
            report_words: 1200,
            max_turns: 30,
            compact_threshold_tokens: 200_000,
            synth_input_budget_tokens: 120_000,
            fetch_url_enabled: false,
            incognito_default: false,
        }
    }
}

impl Config {
    fn path() -> std::path::PathBuf {
        paths::root().join("config.toml")
    }

    pub fn clamp(&mut self) {
        self.default_agents = self.default_agents.clamp(1, 8);
        self.max_parallel = self.max_parallel.clamp(1, 8);
        self.report_words = self.report_words.max(100);
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
