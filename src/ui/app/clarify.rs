use super::*;

impl App {
    pub(super) fn open_clarify(&mut self) {
        let Some(run) = self.run.clone() else { return };
        let (key, n) = run.with(|s| ((s.record.id.clone(), s.record.clarify_rounds), s.record.questions.len()));
        if self.clarify.key != key || self.clarify.answers.len() != n {
            self.clarify = ClarifyForm { key, answers: (0..n).map(|_| answer_ta()).collect(), focus: 0 };
        }
        self.screen = Screen::Clarify;
    }

    pub(super) fn key_clarify(&mut self, k: KeyEvent) {
        let Some(run) = self.run.clone() else {
            self.screen = Screen::Home;
            return;
        };
        if run.status() != RunStatus::Clarifying {
            self.goto_run();
            return;
        }
        let n = self.clarify.answers.len().max(1);
        let focus = self.clarify.focus.min(n - 1);
        match k.code {
            KeyCode::Tab => self.clarify.focus = (focus + 1) % n,
            KeyCode::BackTab => self.clarify.focus = (focus + n - 1) % n,
            KeyCode::Esc => {
                self.modal = Modal::Confirm(Confirm {
                    kind: ConfirmKind::CancelReview,
                    text: "Cancel this research? y/n".into(),
                });
            }
            _ if newline_key(&k) => {
                if let Some(t) = self.clarify.answers.get_mut(focus) {
                    t.insert_newline();
                }
            }
            KeyCode::Enter => {
                let answers = self.clarify.answers.iter().map(|t| t.lines().join("\n")).collect();
                run.answer(answers);
                self.run_sel = 0;
                self.inspect = Inspect::default();
                self.screen = Screen::Run;
            }
            _ => {
                if let Some(t) = self.clarify.answers.get_mut(focus) {
                    t.input(k);
                }
            }
        }
    }
}
