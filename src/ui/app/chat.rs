use super::*;

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
}
