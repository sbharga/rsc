use super::fmt::osc52_copy;
use super::*;

impl App {
    pub(super) fn key_signin(&mut self, k: KeyEvent) {
        let existing = self.signin.existing.clone();
        match &self.signin.phase {
            SignInPhase::Idle | SignInPhase::Error(_) => match k.code {
                KeyCode::Enter => self.begin_sign_in(existing, false),
                KeyCode::Esc => {
                    if let Some(s) = self.signin.return_to.take() {
                        self.screen = s;
                    } else {
                        self.quit = true;
                    }
                }
                _ => {}
            },
            SignInPhase::Waiting { url, cancel } => match k.code {
                KeyCode::Esc => cancel.cancel(),
                KeyCode::Char('c') => {
                    if let Some(u) = url {
                        osc52_copy(u);
                        self.toast("URL copied");
                    }
                }
                _ => {}
            },
            SignInPhase::NotEnabled => match k.code {
                KeyCode::Enter => self.begin_sign_in(existing, true),
                KeyCode::Esc => {
                    if let Some(s) = self.signin.return_to.take() {
                        self.signin.phase = SignInPhase::Idle;
                        self.screen = s;
                    } else {
                        self.quit = true;
                    }
                }
                _ => {}
            },
        }
    }
}
