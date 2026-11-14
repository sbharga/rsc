use crate::paths;
use crate::research::store::{RunConfig, RunMode};
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
    /// Mode the Home Mode chip starts in and `rsc run` uses without --discover.
    pub mode: RunMode,
    /// Named run profiles, `[presets.<name>]`.
    pub presets: std::collections::BTreeMap<String, Preset>,
}

/// Named run profile in `[presets.<name>]`; unset fields keep the config value.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preset {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agents: Option<u8>,
    /// Effort for planner, researcher and synthesizer (models stay as configured; slugs differ per account).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report_words: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gap_round: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify_citations: Option<bool>,
}

impl Preset {
    /// `sm` with this preset's effort.
    pub fn stage(&self, sm: &StageModel) -> StageModel {
        StageModel { model: sm.model.clone(), effort: self.effort.clone().unwrap_or_else(|| sm.effort.clone()) }
    }

    /// Settings the Home chips don't show: report_words, max_turns, gap_round, verify_citations.
    pub fn apply(&self, rc: &mut RunConfig) {
        if let Some(v) = self.report_words {
            rc.report_words = v;
        }
        if let Some(v) = self.max_turns {
            rc.max_turns = v;
        }
        if let Some(v) = self.gap_round {
            rc.gap_round = v;
        }
        if let Some(v) = self.verify_citations {
            rc.verify_citations = v;
        }
    }
}

fn default_presets() -> std::collections::BTreeMap<String, Preset> {
    let p = |agents, effort: &str, report_words, max_turns, gap_round| Preset {
        agents: Some(agents),
        effort: Some(effort.into()),
        report_words,
        max_turns,
        gap_round,
        verify_citations: None,
    };
    [
        ("quick".to_string(), p(2, "low", Some(500), Some(10), None)),
        ("standard".to_string(), p(0, "medium", None, None, None)),
        ("deep".to_string(), p(6, "high", None, Some(50), Some(true))),
    ]
    .into()
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
            mode: RunMode::Research,
            presets: default_presets(),
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
        for p in self.presets.values_mut() {
            if let Some(n) = p.agents.filter(|&n| n != 0) {
                p.agents = Some(n.clamp(1, MAX_AGENTS));
            }
            if let Some(n) = p.report_words.filter(|&n| n != 0) {
                p.report_words = Some(n.max(100));
            }
            p.max_turns = p.max_turns.map(|n| n.max(1));
        }
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
            preset: String::new(),
            report_style: self.report_style.trim().to_string(),
            max_run_tokens: self.max_run_tokens,
            max_run_cost_usd: self.max_run_cost_usd,
            check_links: self.check_links,
            verify_citations: self.verify_citations,
            gap_round: self.gap_round,
            mode: self.mode,
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

    #[test]
    fn clamp_limits_presets() {
        let mut c = Config::default();
        c.presets.insert(
            "x".into(),
            Preset { agents: Some(200), report_words: Some(5), max_turns: Some(0), ..Preset::default() },
        );
        c.presets.insert("auto".into(), Preset { agents: Some(0), report_words: Some(0), ..Preset::default() });
        c.clamp();
        let x = &c.presets["x"];
        assert_eq!((x.agents, x.report_words, x.max_turns), (Some(MAX_AGENTS), Some(100), Some(1)));
        let a = &c.presets["auto"];
        assert_eq!((a.agents, a.report_words), (Some(0), Some(0)));
    }

    #[test]
    fn preset_overrides_only_what_it_sets() {
        let c = Config::default();
        let p = Preset { effort: Some("low".into()), max_turns: Some(7), gap_round: Some(true), ..Preset::default() };
        let sm = StageModel { model: "m".into(), effort: "high".into() };
        assert_eq!(p.stage(&sm), StageModel { model: "m".into(), effort: "low".into() });
        assert_eq!(Preset::default().stage(&sm), sm);
        let mut rc = c.run_config(sm.clone(), sm.clone(), sm.clone(), sm, 3);
        p.apply(&mut rc);
        assert_eq!((rc.max_turns, rc.gap_round, rc.report_words), (7, true, c.report_words));
    }

    #[test]
    fn default_presets_round_trip_through_toml() {
        let c = Config::default();
        let s = toml::to_string_pretty(&c).unwrap();
        assert!(s.contains("[presets.quick]"));
        assert_eq!(toml::from_str::<Config>(&s).unwrap().presets, c.presets);
    }
}
