use super::*;
use crate::config::{MAX_AGENTS, Preset};
use crate::research::context;

/// Next preset in name order: none → first → … → last → none. An unknown current preset resets to none.
fn next_preset(names: &[String], cur: Option<&str>) -> Option<String> {
    match cur {
        None => names.first().cloned(),
        Some(c) => {
            let i = names.iter().position(|n| n == c)?;
            names.get(i + 1).cloned()
        }
    }
}

impl App {
    pub(super) fn key_home(&mut self, k: KeyEvent) {
        if k.code == KeyCode::Tab {
            self.home.chips = !self.home.chips;
            return;
        }
        if !self.home.chips {
            let newline = newline_key(&k);
            if newline {
                self.home.input.insert_newline();
            } else if k.code == KeyCode::Enter {
                self.start_run();
            } else {
                self.home.input.input(k);
            }
            return;
        }
        match k.code {
            KeyCode::Left => self.home.chip = self.home.chip.saturating_sub(1),
            KeyCode::Right => self.home.chip = (self.home.chip + 1).min(5),
            KeyCode::Esc => self.home.chips = false,
            KeyCode::Char('+') | KeyCode::Char('=') if self.home.chip == 0 => {
                self.home.agents = (self.home.agents + 1).min(MAX_AGENTS)
            }
            KeyCode::Char('-') if self.home.chip == 0 => self.home.agents = self.home.agents.saturating_sub(1),
            KeyCode::Char('m') => open_url(USAGE_URL),
            KeyCode::Char('r') => {
                if matches!(self.models, Models::Failed(_)) {
                    self.fetch_models();
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => match self.home.chip {
                0 if k.code == KeyCode::Enter => {
                    self.home.agents = if self.home.agents >= MAX_AGENTS { 0 } else { self.home.agents + 1 }
                }
                1..=3 if k.code == KeyCode::Enter => {
                    let stage = [Stage::Planner, Stage::Researcher, Stage::Synthesizer][self.home.chip - 1];
                    self.modal = Modal::Picker(Picker {
                        scope: PickScope::Chip,
                        stage,
                        effort_step: false,
                        chosen: None,
                        sel: 0,
                    });
                    self.picker_preselect();
                }
                4 => self.cycle_preset(),
                5 if k.code == KeyCode::Enter => {
                    let ta = single_line_ta(&format!("{}/", self.cwd.display()));
                    self.modal = Modal::Attach(Box::new(PathInput { ta, err: None }));
                }
                _ => {}
            },
            KeyCode::Char('x') if self.home.chip == 5 => {
                self.home.context.clear();
                self.toast("Attached files removed");
            }
            _ => {}
        }
    }

    /// Next preset (or none); copies the preset's agents and effort onto the chips.
    fn cycle_preset(&mut self) {
        let names: Vec<String> = self.cfg.presets.keys().cloned().collect();
        self.home.preset = next_preset(&names, self.home.preset.as_deref());
        let p =
            self.home.preset.as_ref().and_then(|n| self.cfg.presets.get(n)).cloned().unwrap_or_else(Preset::default);
        self.home.agents = p.agents.unwrap_or(self.cfg.default_agents);
        self.home.planner = p.stage(&self.cfg.planner);
        self.home.researcher = p.stage(&self.cfg.researcher);
        self.home.synthesizer = p.stage(&self.cfg.synthesizer);
    }

    /// Load a file in the background; the result arrives as `AppMsg::Context`.
    pub(super) fn attach(&mut self, path: PathBuf) {
        let tx = self.msg_tx.clone();
        tokio::spawn(async move {
            let r = tokio::task::spawn_blocking(move || context::load(&path))
                .await
                .unwrap_or_else(|_| Err(anyhow::anyhow!("Couldn't read the file")));
            let _ = tx.send(AppMsg::Context(r.map_err(|e| e.to_string())));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_cycle_through_names_and_back_to_none() {
        let names: Vec<String> = ["a", "b"].map(String::from).to_vec();
        assert_eq!(next_preset(&names, None).as_deref(), Some("a"));
        assert_eq!(next_preset(&names, Some("a")).as_deref(), Some("b"));
        assert_eq!(next_preset(&names, Some("b")), None);
        assert_eq!(next_preset(&names, Some("gone")), None);
        assert_eq!(next_preset(&[], None), None);
    }
}
