use super::*;
use crate::config::MAX_AGENTS;
use crate::research::store;

impl App {
    /// Open the follow-up chat for the report in the viewer.
    pub(super) fn open_chat(&mut self) {
        let Some((rec, report)) = self.viewer.chat.clone() else {
            self.toast("Chat opens once the final report is ready");
            return;
        };
        if self.chat.as_ref().is_some_and(|c| c.chat.id() == rec.id) {
            self.screen = Screen::Chat;
            return;
        }
        let Some(ctx) = self.ctx() else { return };
        if let Some(old) = self.chat.take() {
            old.chat.cancel.cancel();
        }
        self.chat = Some(ChatView::new(Chat::open(ctx, rec, report)));
        self.screen = Screen::Chat;
    }

    pub(super) fn key_chat(&mut self, k: KeyEvent) {
        if ctrl(&k, 't') {
            self.confirm_deepen();
            return;
        }
        let Some(cv) = self.chat.as_mut() else {
            self.screen = Screen::Home;
            return;
        };
        let page = (cv.view_h as usize).saturating_sub(1).max(1);
        match k.code {
            _ if newline_key(&k) => cv.input.insert_newline(),
            KeyCode::Enter => {
                let q = cv.input.lines().join("\n").trim().to_string();
                if q.is_empty() {
                    return;
                }
                if cv.chat.busy() {
                    self.toast("Wait for the current answer");
                    return;
                }
                cv.chat.send(q);
                cv.input.select_all();
                cv.input.cut();
                cv.scroll = 0;
            }
            KeyCode::PageUp => cv.scroll = cv.scroll.saturating_add(page),
            KeyCode::PageDown => cv.scroll = cv.scroll.saturating_sub(page),
            KeyCode::Esc => self.screen = Screen::Viewer,
            _ => {
                cv.input.input(k);
            }
        }
    }

    /// Ctrl+T: confirm running the angles the last answer proposed.
    fn confirm_deepen(&mut self) {
        let Some(cv) = &self.chat else { return };
        let (busy, proposal, id) = (cv.chat.busy(), cv.chat.proposal(), cv.chat.id().to_string());
        if busy {
            self.toast("Wait for the current answer");
            return;
        }
        let Some((question, angles)) = proposal else {
            self.toast("No research proposed");
            return;
        };
        if self.run_live() {
            self.toast("A research is already running — Ctrl+G");
            return;
        }
        let rec = match store::load(&id) {
            Ok(r) => r,
            Err(e) => {
                self.toast(format!("Couldn't load research: {e}"));
                return;
            }
        };
        if rec.status != RunStatus::Done {
            self.toast("Research is not done");
        } else if rec.budget_hit || rec.over_budget() {
            self.toast("This research hit its budget cap");
        } else if rec.agents.len() + angles.len() > MAX_AGENTS as usize {
            self.toast(format!("Maximum {MAX_AGENTS} agents"));
        } else {
            let n = angles.len();
            self.modal = Modal::Confirm(Confirm {
                text: format!("Run {n} researcher{} and write a new revision? y/n", if n == 1 { "" } else { "s" }),
                kind: ConfirmKind::Deepen { id, question, angles },
            });
        }
    }

    /// Start the deep-dive: new researchers, then a new report revision.
    pub(super) fn deepen(&mut self, id: &str, question: String, angles: Vec<Perspective>) {
        let Some(ctx) = self.ctx() else { return };
        let rec = match store::load(id) {
            Ok(r) => r,
            Err(e) => {
                self.toast(format!("Couldn't load research: {e}"));
                return;
            }
        };
        // The next chat must open on the new report.
        if let Some(c) = self.chat.take() {
            c.chat.cancel.cancel();
        }
        self.run = Some(Run::deepen(ctx, rec, question, angles));
        self.run_sel = 0;
        self.inspect = Inspect::default();
        self.screen = Screen::Run;
    }
}
