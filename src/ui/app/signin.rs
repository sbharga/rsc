use super::fmt::osc52_copy;
use super::*;

impl App {
    pub(super) fn key_signin(&mut self, k: KeyEvent) {
        let existing = self.signin.existing.clone();
        match &mut self.signin.phase {
            SignInPhase::Idle => match k.code {
                KeyCode::Up | KeyCode::Down | KeyCode::Tab | KeyCode::BackTab => {
                    self.signin.use_key = !self.signin.use_key
                }
                KeyCode::Enter => {
                    if self.signin.use_key {
                        self.open_key_entry(None);
                    } else {
                        self.begin_sign_in(existing, false);
                    }
                }
                KeyCode::Esc => {
                    if let Some(s) = self.signin.return_to.take() {
                        self.screen = s;
                    } else {
                        self.quit = true;
                    }
                }
                _ => {}
            },
            SignInPhase::Error(_) => match k.code {
                KeyCode::Enter => self.begin_sign_in(existing, false),
                KeyCode::Esc => {
                    self.signin.phase = SignInPhase::Idle;
                    if let Some(s) = self.signin.return_to.take() {
                        self.screen = s;
                    }
                }
                _ => {}
            },
            SignInPhase::KeyEntry(ke) => match k.code {
                KeyCode::Enter => {
                    let typed = ke.ta.lines().concat().trim().to_string();
                    if !typed.is_empty() {
                        self.check_key(typed, true);
                    } else if let Some(env) = auth::env_api_key() {
                        self.check_key(env, false);
                    } else {
                        ke.err = Some("Paste an API key.".into());
                    }
                }
                KeyCode::Esc => {
                    self.signin.phase = SignInPhase::Idle;
                    if let Some(s) = self.signin.return_to.take() {
                        self.screen = s;
                    }
                }
                _ => {
                    ke.ta.input(k);
                }
            },
            SignInPhase::Checking => {}
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
                    self.signin.phase = SignInPhase::Idle;
                    if let Some(s) = self.signin.return_to.take() {
                        self.screen = s;
                    }
                }
                _ => {}
            },
        }
    }
}
