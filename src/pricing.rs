use crate::paths;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ops::AddAssign;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Usage {
    pub input: u64,
    pub cached: u64,
    pub output: u64,
    pub reasoning: u64,
    pub web_search_calls: u64,
}

impl AddAssign for Usage {
    fn add_assign(&mut self, o: Usage) {
        self.input += o.input;
        self.cached += o.cached;
        self.output += o.output;
        self.reasoning += o.reasoning;
        self.web_search_calls += o.web_search_calls;
    }
}

impl Usage {
    pub fn tokens(&self) -> u64 {
        self.input + self.output
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Rates {
    pub input: f64,
    pub cached_input: f64,
    pub output: f64,
    pub long_input: f64,
    pub long_cached_input: f64,
    pub long_output: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Pricing {
    pub web_search_per_1k: f64,
    pub long_context_threshold: u64,
    #[serde(default)]
    pub models: BTreeMap<String, Rates>,
}

fn rates(i: f64, ci: f64, o: f64, li: f64, lci: f64, lo: f64) -> Rates {
    Rates { input: i, cached_input: ci, output: o, long_input: li, long_cached_input: lci, long_output: lo }
}

impl Default for Pricing {
    fn default() -> Self {
        let mut models = BTreeMap::new();
        models.insert("gpt-6-astra".into(), rates(10.0, 1.0, 50.0, 20.0, 2.0, 75.0));
        models.insert("gpt-6.1-sol".into(), rates(2.0, 0.10, 10.0, 4.0, 0.20, 15.0));
        models.insert("gpt-6.1-terra".into(), rates(1.0, 0.05, 5.0, 2.0, 0.10, 7.5));
        models.insert("gpt-6-luna".into(), rates(0.10, 0.01, 0.50, 0.20, 0.02, 0.75));
        models.insert("gpt-6-sol".into(), rates(2.0, 0.20, 10.0, 4.0, 0.40, 15.0));
        Self { web_search_per_1k: 10.0, long_context_threshold: 272_000, models }
    }
}

impl Pricing {
    fn path() -> std::path::PathBuf {
        paths::root().join("pricing.toml")
    }

    /// Load pricing.toml, seeding it on first run. Invalid file → defaults (file untouched).
    pub fn load() -> Pricing {
        match std::fs::read_to_string(Self::path()) {
            Ok(s) => toml::from_str(&s).unwrap_or_default(),
            Err(_) => {
                let p = Pricing::default();
                if let Ok(s) = toml::to_string_pretty(&p) {
                    let _ = paths::atomic_write(&Self::path(), s.as_bytes());
                }
                p
            }
        }
    }

    /// Cost in USD; `None` if the model has no known rates.
    pub fn cost(&self, model: &str, u: &Usage) -> Option<f64> {
        let r = self.models.get(model)?;
        let long = u.input > self.long_context_threshold;
        let (i, c, o) = if long {
            (r.long_input, r.long_cached_input, r.long_output)
        } else {
            (r.input, r.cached_input, r.output)
        };
        let fresh = u.input.saturating_sub(u.cached) as f64;
        let tok = (fresh * i + u.cached as f64 * c + u.output as f64 * o) / 1e6;
        Some(tok + self.search_cost(u))
    }

    pub fn search_cost(&self, u: &Usage) -> f64 {
        u.web_search_calls as f64 * self.web_search_per_1k / 1000.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_context() {
        let p = Pricing::default();
        let u = Usage { input: 1_000_000, cached: 500_000, output: 100_000, web_search_calls: 10, ..Default::default() };
        // input > 272k threshold → long rates; compute explicitly below instead
        let c = p.cost("gpt-6.1-sol", &u).unwrap();
        // long: 0.5*4 + 0.5*0.2 + 0.1*15 + 0.1
        assert!((c - (2.0 + 0.1 + 1.5 + 0.1)).abs() < 1e-9, "{c}");
    }

    #[test]
    fn short_rates_under_threshold() {
        let p = Pricing::default();
        let u = Usage { input: 200_000, cached: 100_000, output: 100_000, web_search_calls: 10, ..Default::default() };
        let c = p.cost("gpt-6.1-sol", &u).unwrap();
        assert!((c - (0.1 * 2.0 + 0.1 * 0.10 + 0.1 * 10.0 + 0.1)).abs() < 1e-9, "{c}");
    }

    #[test]
    fn long_context() {
        let p = Pricing::default();
        let u = Usage { input: 300_000, cached: 0, output: 100_000, web_search_calls: 10, ..Default::default() };
        let c = p.cost("gpt-6.1-sol", &u).unwrap();
        assert!((c - 2.80).abs() < 1e-9, "{c}");
    }

    #[test]
    fn unknown_model() {
        let p = Pricing::default();
        assert!(p.cost("nope", &Usage::default()).is_none());
    }
}
