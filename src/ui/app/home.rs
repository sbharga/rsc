use super::*;
use crate::config::MAX_AGENTS;

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
            KeyCode::Right => self.home.chip = (self.home.chip + 1).min(3),
            KeyCode::Esc => self.home.chips = false,
            KeyCode::Char('+') | KeyCode::Char('=') if self.home.chip == 0 => {
                self.home.agents = (self.home.agents + 1).min(MAX_AGENTS)
            }
            KeyCode::Char('-') if self.home.chip == 0 => self.home.agents = self.home.agents.saturating_sub(1).max(1),
            KeyCode::Char('m') => open_url(USAGE_URL),
            KeyCode::Char('r') => {
                if matches!(self.models, Models::Failed(_)) {
                    self.fetch_models();
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => match self.home.chip {
                0 if k.code == KeyCode::Enter => {
                    self.home.agents = if self.home.agents >= MAX_AGENTS { 1 } else { self.home.agents + 1 }
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
                _ => {}
            },
            _ => {}
        }
    }
}
