use super::fmt::expand_tilde;
use super::review::upsert_angle;
use super::*;
use crate::research::store::{self, Perspective};

impl App {
    pub(super) fn modal_key(&mut self, m: Modal, k: KeyEvent) -> Modal {
        match m {
            Modal::None => Modal::None,
            Modal::Welcome => {
                if k.code == KeyCode::Enter {
                    if let Some(a) = &mut self.account {
                        a.welcomed = true;
                        let _ = a.save();
                    }
                    Modal::None
                } else {
                    Modal::Welcome
                }
            }
            Modal::UsageLimit => match k.code {
                KeyCode::Enter => {
                    open_url(USAGE_URL);
                    Modal::None
                }
                KeyCode::Esc => Modal::None,
                _ => Modal::UsageLimit,
            },
            Modal::Detail(lines) => match k.code {
                KeyCode::Esc | KeyCode::Enter => Modal::None,
                KeyCode::Char('r') => {
                    if let Some(rec) = self.selected_history()
                        && rec.status.is_resumable()
                    {
                        self.resume_from_history(rec);
                        return Modal::None;
                    }
                    Modal::Detail(lines)
                }
                _ => Modal::Detail(lines),
            },
            Modal::Picker(p) => self.picker_key(p, k),
            Modal::Angle(a) => self.angle_key(a, k),
            Modal::Confirm(c) => self.confirm_key(c, k),
            Modal::Save(s) => self.save_key(s, k),
            Modal::Style(s) => self.style_key(s, k),
            Modal::Number(n) => self.number_key(n, k),
            Modal::Account(items, sel) => self.account_key(items, sel, k),
            Modal::Help => Modal::None,
        }
    }

    pub(super) fn picker_key(&mut self, mut p: Picker, k: KeyEvent) -> Modal {
        if !matches!(self.models, Models::Ready(_)) {
            match k.code {
                KeyCode::Esc => return Modal::None,
                KeyCode::Char('r') if matches!(self.models, Models::Failed(_)) => self.fetch_models(),
                _ => {}
            }
            return Modal::Picker(p);
        }
        let entries = self.picker_entries(&p);
        match k.code {
            KeyCode::Up => p.sel = p.sel.saturating_sub(1),
            KeyCode::Down => p.sel = (p.sel + 1).min(entries.len().saturating_sub(1)),
            KeyCode::Esc => {
                if p.effort_step {
                    p.effort_step = false;
                    p.chosen = None;
                    let sel = self.picker_entries(&p).iter().position(|(_, c)| *c).unwrap_or(0);
                    p.sel = sel;
                } else {
                    return Modal::None;
                }
            }
            KeyCode::Enter => {
                if p.effort_step {
                    let effort = entries.get(p.sel).map(|e| e.0.clone()).unwrap_or_else(|| "default".into());
                    let model = p.chosen.clone().unwrap_or_default();
                    self.set_stage(p.scope, p.stage, model, effort);
                    return Modal::None;
                }
                let Some(m) = self.catalog().get(p.sel) else { return Modal::Picker(p) };
                p.chosen = Some(m.slug.clone());
                p.effort_step = true;
                let cur = self.current_stage(p.scope, p.stage);
                p.sel = self.picker_entries(&p).iter().position(|(e, _)| *e == cur.effort).unwrap_or(0);
            }
            _ => {}
        }
        Modal::Picker(p)
    }

    pub(super) fn picker_preselect(&mut self) {
        if let Modal::Picker(p) = &self.modal {
            let sel = self.picker_entries(p).iter().position(|(_, c)| *c).unwrap_or(0);
            if let Modal::Picker(p) = &mut self.modal {
                p.sel = sel;
            }
        }
    }

    pub(super) fn angle_key(&mut self, mut a: Box<AngleEdit>, k: KeyEvent) -> Modal {
        match k.code {
            KeyCode::Esc => return Modal::None,
            KeyCode::Tab | KeyCode::BackTab => a.focus_brief = !a.focus_brief,
            KeyCode::Enter => {
                let title = a.title.lines().join(" ").trim().to_string();
                let brief = a.brief.lines().join(" ").trim().to_string();
                if title.is_empty() {
                    a.focus_brief = false;
                    return Modal::Angle(a);
                }
                if let Some(run) = &self.run {
                    let mut ps = run.with(|s| s.record.perspectives.clone());
                    if let Some(sel) = upsert_angle(&mut ps, a.idx, Perspective { title, brief }) {
                        self.review_sel = sel;
                    }
                    run.set_perspectives(ps);
                }
                return Modal::None;
            }
            _ => {
                if a.focus_brief {
                    a.brief.input(k);
                } else {
                    a.title.input(k);
                }
            }
        }
        Modal::Angle(a)
    }

    pub(super) fn confirm_key(&mut self, c: Confirm, k: KeyEvent) -> Modal {
        match k.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                match c.kind {
                    ConfirmKind::CancelReview => {
                        if let Some(r) = &self.run {
                            r.cancel();
                        }
                        self.screen = Screen::Home;
                    }
                    ConfirmKind::CancelRun => {
                        if let Some(r) = &self.run {
                            r.cancel();
                        }
                    }
                    ConfirmKind::Delete(id) => match store::delete(&id) {
                        Ok(()) => {
                            if self.run.as_ref().is_some_and(|r| r.with(|s| s.record.id == id)) {
                                self.run = None;
                            }
                            if self.chat.as_ref().is_some_and(|c| c.chat.id() == id)
                                && let Some(c) = self.chat.take()
                            {
                                c.chat.cancel.cancel();
                            }
                            self.history.items.retain(|r| r.id != id);
                            let n = self.history.visible().len();
                            self.history.sel = self.history.sel.min(n.saturating_sub(1));
                            self.toast("Deleted");
                        }
                        Err(e) => self.toast(format!("Delete failed: {e}")),
                    },
                    ConfirmKind::Overwrite { path, text } => self.do_save(path, &text),
                }
                Modal::None
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Modal::None,
            _ => Modal::Confirm(c),
        }
    }

    pub(super) fn save_key(&mut self, mut s: Box<SaveModal>, k: KeyEvent) -> Modal {
        match k.code {
            KeyCode::Esc => Modal::None,
            KeyCode::Enter => {
                let raw = s.ta.lines().join("");
                let path = expand_tilde(raw.trim());
                if raw.trim().is_empty() {
                    return Modal::Save(s);
                }
                let parent =
                    path.parent().map(|p| if p.as_os_str().is_empty() { PathBuf::from(".") } else { p.to_path_buf() });
                if !parent.is_some_and(|p| p.is_dir()) {
                    s.err = Some("Folder doesn't exist".into());
                    return Modal::Save(s);
                }
                if path.exists() {
                    return Modal::Confirm(Confirm {
                        text: format!("Overwrite {}? y/n", path.display()),
                        kind: ConfirmKind::Overwrite { path, text: s.text },
                    });
                }
                self.do_save(path, &s.text);
                Modal::None
            }
            _ => {
                s.err = None;
                s.ta.input(k);
                Modal::Save(s)
            }
        }
    }

    pub(super) fn number_key(&mut self, mut n: Box<NumberEdit>, k: KeyEvent) -> Modal {
        match k.code {
            KeyCode::Esc => Modal::None,
            KeyCode::Enter => {
                let txt = n.ta.lines().join("");
                let Ok(v) = txt.trim().parse::<u32>() else {
                    n.err = Some("Enter a number".into());
                    return Modal::Number(n);
                };
                let v = n.which.clamp(v);
                match n.which {
                    Num::DefaultAgents => {
                        self.cfg.default_agents = v as u8;
                        self.home.agents = self.cfg.default_agents;
                    }
                    Num::MaxParallel => self.cfg.max_parallel = v as u8,
                    Num::ReportWords => self.cfg.report_words = v,
                }
                self.save_cfg();
                Modal::None
            }
            _ => {
                n.err = None;
                if !matches!(k.code, KeyCode::Char(c) if !c.is_ascii_digit()) {
                    n.ta.input(k);
                }
                Modal::Number(n)
            }
        }
    }

    pub(super) fn style_key(&mut self, mut s: Box<StyleEdit>, k: KeyEvent) -> Modal {
        if k.code == KeyCode::Esc {
            return Modal::None;
        }
        if newline_key(&k) {
            s.ta.insert_newline();
        } else if k.code == KeyCode::Enter {
            self.cfg.report_style = s.ta.lines().join("\n").trim().to_string();
            self.save_cfg();
            return Modal::None;
        } else {
            s.ta.input(k);
        }
        Modal::Style(s)
    }

    pub(super) fn account_key(&mut self, items: Vec<(String, AcctAction)>, mut sel: usize, k: KeyEvent) -> Modal {
        match k.code {
            KeyCode::Esc => return Modal::None,
            KeyCode::Up => sel = sel.saturating_sub(1),
            KeyCode::Down => sel = (sel + 1).min(items.len().saturating_sub(1)),
            KeyCode::Enter => {
                match items[sel].1.clone() {
                    AcctAction::Add => {
                        self.signin.return_to = Some(Screen::Settings);
                        self.begin_sign_in(None, false);
                    }
                    AcctAction::Enable => {
                        self.signin.return_to = Some(Screen::Settings);
                        let ex = self.account.clone();
                        self.begin_sign_in(ex, true);
                    }
                    AcctAction::Switch(id) => {
                        if let Ok(a) = Account::load(&id) {
                            let _ = auth::set_active_account(Some(&id));
                            if let Some(r) = &self.run {
                                r.cancel();
                            }
                            if let Some(c) = self.chat.take() {
                                c.chat.cancel.cancel();
                            }
                            if a.has_tokens() && a.plan_usage_enabled {
                                self.models = Models::Loading;
                                self.activate(a);
                            } else {
                                self.signin.return_to = Some(Screen::Settings);
                                self.begin_sign_in(Some(a), false);
                            }
                        }
                    }
                    AcctAction::SignOut => {
                        if let Some(a) = self.account.clone() {
                            if let Some(r) = &self.run {
                                r.cancel();
                            }
                            if let Some(c) = self.chat.take() {
                                c.chat.cancel.cancel();
                            }
                            let tx = self.msg_tx.clone();
                            let id = a.id();
                            tokio::spawn(async move {
                                let ok = auth::sign_out(&id).await;
                                let _ = tx.send(AppMsg::SignedOut(ok));
                            });
                            let mut a = a;
                            a.access_token = None;
                            a.refresh_token = None;
                            a.id_token = None;
                            self.signin.existing = Some(a);
                            self.signin.phase = SignInPhase::Idle;
                            self.signin.return_to = None;
                            self.account = None;
                            self.client = None;
                            self.models = Models::Loading;
                            self.run = None;
                            self.screen = Screen::SignIn;
                        }
                    }
                }
                return Modal::None;
            }
            _ => {}
        }
        Modal::Account(items, sel)
    }
}
