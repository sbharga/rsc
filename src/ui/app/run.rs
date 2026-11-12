use super::*;

impl App {
    pub(super) fn key_run(&mut self, k: KeyEvent) {
        let Some(run) = self.run.clone() else {
            self.screen = Screen::Home;
            return;
        };
        let (n, status) = run.with(|s| (s.record.agents.len(), s.record.status));
        let page = self.inspect.view_h.max(1);
        match k.code {
            KeyCode::Up => {
                self.run_sel = self.run_sel.saturating_sub(1);
                self.inspect.scroll = 0;
            }
            KeyCode::Down => {
                self.run_sel = (self.run_sel + 1).min(n.saturating_sub(1));
                self.inspect.scroll = 0;
            }
            KeyCode::Char('i') if n > 0 => {
                self.inspect.full = !self.inspect.full;
                self.inspect.scroll = 0;
            }
            KeyCode::PageUp => self.inspect.scroll += page,
            KeyCode::PageDown => self.inspect.scroll = self.inspect.scroll.saturating_sub(page),
            KeyCode::End => self.inspect.scroll = 0,
            KeyCode::Enter => {
                let has = run.with(|s| s.reports.get(self.run_sel).is_some_and(|r| r.is_some()));
                if has {
                    self.open_run_viewer(Some(self.run_sel), Screen::Run);
                }
            }
            KeyCode::Char('x') if status.is_live() => {
                self.modal =
                    Modal::Confirm(Confirm { kind: ConfirmKind::CancelRun, text: "Cancel this research? y/n".into() });
            }
            KeyCode::Char('r') if status.is_resumable() => {
                let (rec, mem) = run.with(|s| (s.record.clone(), s.reports.clone()));
                self.resume_record(rec, mem);
            }
            KeyCode::Esc => {
                if self.inspect.full {
                    self.inspect.full = false;
                } else {
                    self.screen = Screen::Home;
                }
            }
            _ => {}
        }
    }
}
