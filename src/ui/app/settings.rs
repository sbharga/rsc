use super::*;
use crate::config::MAX_AGENTS;

impl App {
    pub(super) fn key_settings(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Esc => self.screen = Screen::Home,
            KeyCode::Up => self.settings_sel = self.settings_sel.saturating_sub(1),
            KeyCode::Down => self.settings_sel = (self.settings_sel + 1).min(ROWS.len() - 1),
            KeyCode::Enter | KeyCode::Char(' ') => match ROWS[self.settings_sel] {
                Row::Account => {
                    let mut items: Vec<(String, AcctAction)> = vec![];
                    let active = self.account.as_ref().map(|a| a.id());
                    for a in auth::list_accounts() {
                        if Some(a.id()) != active {
                            items.push((format!("Switch to {} · {}", a.email, a.id()), AcctAction::Switch(a.id())));
                        }
                    }
                    items.push(("Add account".into(), AcctAction::Add));
                    if self.account.as_ref().is_some_and(|a| a.has_tokens()) {
                        items.push(("Sign out".into(), AcctAction::SignOut));
                    }
                    if self.account.as_ref().is_some_and(|a| !a.plan_usage_enabled) {
                        items.push(("Enable ChatGPT plan usage".into(), AcctAction::Enable));
                    }
                    self.modal = Modal::Account(items, 0);
                }
                Row::Usage => open_url(USAGE_URL),
                Row::Model(stage) if k.code == KeyCode::Enter => {
                    self.modal = Modal::Picker(Picker {
                        scope: PickScope::Settings,
                        stage,
                        effort_step: false,
                        chosen: None,
                        sel: 0,
                    });
                    self.picker_preselect();
                }
                Row::Model(_) => {}
                Row::Num(which) if k.code == KeyCode::Enter => {
                    let cur = match which {
                        Num::DefaultAgents => self.cfg.default_agents as u32,
                        Num::MaxParallel => self.cfg.max_parallel as u32,
                        Num::ReportWords => self.cfg.report_words,
                    };
                    self.modal =
                        Modal::Number(Box::new(NumberEdit { which, ta: single_line_ta(&cur.to_string()), err: None }));
                }
                Row::Num(_) => {}
                Row::ReportStyle if k.code == KeyCode::Enter => {
                    let mut lines: Vec<String> = self.cfg.report_style.lines().map(String::from).collect();
                    if lines.is_empty() {
                        lines.push(String::new());
                    }
                    let mut ta = TextArea::new(lines);
                    ta.set_placeholder_text("e.g. Explain like I'm 5; avoid jargon");
                    ta.set_placeholder_style(Style::new().fg(MUTED));
                    ta.set_cursor_line_style(Style::default());
                    ta.set_wrap_mode(WrapMode::WordOrGlyph);
                    ta.move_cursor(ratatui_textarea::CursorMove::Bottom);
                    ta.move_cursor(ratatui_textarea::CursorMove::End);
                    self.modal = Modal::Style(Box::new(StyleEdit { ta }));
                }
                Row::ReportStyle => {}
                Row::FetchUrl => {
                    self.cfg.fetch_url_enabled = !self.cfg.fetch_url_enabled;
                    self.save_cfg();
                }
                Row::CheckLinks => {
                    self.cfg.check_links = !self.cfg.check_links;
                    self.save_cfg();
                }
                Row::VerifyCitations => {
                    self.cfg.verify_citations = !self.cfg.verify_citations;
                    self.save_cfg();
                }
                Row::GapRound => {
                    self.cfg.gap_round = !self.cfg.gap_round;
                    self.save_cfg();
                }
                Row::Mode => {
                    self.cfg.mode = self.cfg.mode.toggled();
                    self.home.mode = self.cfg.mode;
                    self.save_cfg();
                }
                Row::CheckUpdates => {
                    self.cfg.check_updates = !self.cfg.check_updates;
                    self.save_cfg();
                }
            },
            _ => {}
        }
    }
}

impl Num {
    /// Limit an entered value to what the setting accepts; default agents and report words keep 0 as "auto".
    pub fn clamp(self, v: u32) -> u32 {
        match self {
            Num::DefaultAgents if v == 0 => 0,
            Num::DefaultAgents | Num::MaxParallel => v.clamp(1, MAX_AGENTS as u32),
            Num::ReportWords if v == 0 => 0,
            Num::ReportWords => v.clamp(100, 20_000),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_counts_clamp_to_max_and_default_keeps_auto() {
        let got: Vec<u32> = [0, 1, 99, 100, 20_001].iter().map(|&v| Num::DefaultAgents.clamp(v)).collect();
        assert_eq!(got, [0, 1, 99, 99, 99]);
        let got: Vec<u32> = [0, 1, 99, 100, 20_001].iter().map(|&v| Num::MaxParallel.clamp(v)).collect();
        assert_eq!(got, [1, 1, 99, 99, 99]);
    }

    #[test]
    fn report_words_keep_zero_and_clamp_otherwise() {
        let got: Vec<u32> = [0, 1, 99, 100, 20_001].iter().map(|&v| Num::ReportWords.clamp(v)).collect();
        assert_eq!(got, [0, 100, 100, 100, 20_000]);
    }
}
