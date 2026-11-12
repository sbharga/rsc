use super::*;
use crate::research::store;

impl App {
    pub fn open_history(&mut self) {
        let mut items = store::list();
        if let Some(run) = &self.run {
            let (id, rec) = run.with(|s| (s.record.id.clone(), s.record.clone()));
            if let Some(slot) = items.iter_mut().find(|r| r.id == id) {
                *slot = rec;
            }
        }
        self.history.items = items;
        self.history.sel = 0;
        self.history.filtering = false;
        self.history.filter.clear();
        self.screen = Screen::History;
    }

    pub(super) fn selected_history(&self) -> Option<Record> {
        self.history.visible().get(self.history.sel).map(|r| (*r).clone())
    }

    pub(super) fn key_history(&mut self, k: KeyEvent) {
        if self.history.filtering {
            self.history.filter_key(k.code);
            return;
        }
        let n = self.history.visible().len();
        match k.code {
            KeyCode::Up => self.history.sel = self.history.sel.saturating_sub(1),
            KeyCode::Down => self.history.sel = (self.history.sel + 1).min(n.saturating_sub(1)),
            KeyCode::Char('/') => self.history.filtering = true,
            KeyCode::Esc => self.screen = Screen::Home,
            KeyCode::Enter => {
                let Some(rec) = self.selected_history() else { return };
                let active_id = self.run.as_ref().map(|r| r.with(|s| s.record.id.clone()));
                if active_id.as_deref() == Some(rec.id.as_str()) {
                    self.goto_run();
                    return;
                }
                match rec.status {
                    RunStatus::Done => self.open_record_viewer(&rec),
                    RunStatus::Review | RunStatus::Clarifying => self.resume_from_history(rec),
                    _ => {
                        let mut lines = vec![format!("{} · {}", rec.status.label(), fmt::one_line(&rec.question))];
                        if let Some(e) = &rec.error {
                            lines.push(e.message.clone());
                            if let Some(r) = &e.request_id {
                                lines.push(format!("request {r}"));
                            }
                        }
                        if rec.status.is_resumable() {
                            lines.push(String::new());
                            lines.push("r resume".into());
                        }
                        self.modal = Modal::Detail(lines);
                    }
                }
            }
            KeyCode::Char('r') => {
                if let Some(rec) = self.selected_history()
                    && rec.status.is_resumable()
                {
                    self.resume_from_history(rec);
                }
            }
            KeyCode::Char('d') => {
                if let Some(rec) = self.selected_history() {
                    let live =
                        self.run.as_ref().is_some_and(|r| r.status().is_live() && r.with(|s| s.record.id == rec.id));
                    if live {
                        self.toast("Can't delete the active research");
                    } else {
                        self.modal = Modal::Confirm(Confirm {
                            kind: ConfirmKind::Delete(rec.id),
                            text: "Delete this research? y/n".into(),
                        });
                    }
                }
            }
            _ => {}
        }
    }
}

impl History {
    /// Records whose question contains the filter (case-insensitive); all when the filter is empty.
    pub fn visible(&self) -> Vec<&Record> {
        let f = self.filter.to_lowercase();
        self.items.iter().filter(|r| f.is_empty() || r.question.to_lowercase().contains(&f)).collect()
    }

    /// A key typed while the filter box is active.
    pub fn filter_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                self.filtering = false;
                self.filter.clear();
            }
            KeyCode::Enter => self.filtering = false,
            KeyCode::Backspace => {
                self.filter.pop();
            }
            KeyCode::Char(c) => self.filter.push(c),
            _ => {}
        }
        self.sel = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(questions: &[&str]) -> History {
        let sm = StageModel { model: "m".into(), effort: "low".into() };
        let items = questions
            .iter()
            .map(|q| {
                Record::new(
                    q.to_string(),
                    Config::default().run_config(sm.clone(), sm.clone(), sm.clone(), sm.clone(), 1),
                )
            })
            .collect();
        History { items, filter: String::new(), filtering: true, sel: 2 }
    }

    fn type_str(h: &mut History, s: &str) {
        for c in s.chars() {
            h.filter_key(KeyCode::Char(c));
        }
    }

    #[test]
    fn visible_matches_case_insensitively() {
        let mut h = history(&["What is RISC-V?", "Rust async", "risc history"]);
        assert_eq!(h.visible().len(), 3);
        type_str(&mut h, "RiSc");
        let q: Vec<_> = h.visible().iter().map(|r| r.question.as_str()).collect();
        assert_eq!(q, ["What is RISC-V?", "risc history"]);
    }

    #[test]
    fn filter_edits_buffer_and_resets_selection() {
        let mut h = history(&["a"]);
        type_str(&mut h, "ab");
        assert_eq!((h.filter.as_str(), h.sel), ("ab", 0));
        h.sel = 3;
        h.filter_key(KeyCode::Backspace);
        assert_eq!((h.filter.as_str(), h.sel), ("a", 0));
        assert!(h.filtering);
    }

    #[test]
    fn escape_clears_filter_and_enter_keeps_it() {
        let mut h = history(&["a"]);
        type_str(&mut h, "ab");
        h.filter_key(KeyCode::Enter);
        assert_eq!((h.filter.as_str(), h.filtering), ("ab", false));
        h.filtering = true;
        h.filter_key(KeyCode::Esc);
        assert_eq!((h.filter.as_str(), h.filtering), ("", false));
    }
}
