use super::fmt::osc52_copy;
use super::*;
use crate::export;

/// "Final", or "Final v2" once a chat deep-dive has produced a newer revision.
fn final_label(rec: &Record) -> String {
    if rec.revision > 1 { format!("Final v{}", rec.revision) } else { "Final".into() }
}

impl App {
    pub(super) fn make_tab(label: String, text: String, agent: Option<usize>) -> ViewTab {
        let rendered = own_text(tui_markdown::from_str(&text));
        ViewTab { label, text, rendered, agent, is_sources: false }
    }

    pub(super) fn sources_tab(sources: &[Source]) -> ViewTab {
        ViewTab {
            label: format!("Sources ({})", sources.len()),
            text: format!("# Sources\n\n{}", research::sources::render_list(sources, |_| true)),
            rendered: Text::default(),
            agent: None,
            is_sources: true,
        }
    }

    pub(super) fn open_viewer(
        &mut self,
        mut tabs: Vec<ViewTab>,
        tab: usize,
        back: Screen,
        question: String,
        sources: Vec<Source>,
        chat: Option<(Record, String)>,
    ) {
        if tabs.is_empty() {
            self.toast("Nothing to show yet");
            return;
        }
        if !sources.is_empty() {
            tabs.push(Self::sources_tab(&sources));
        }
        self.viewer =
            Viewer { tabs, tab, scroll: 0, back, question, view_h: 10, max_scroll: 0, sources, src_sel: 0, chat };
        self.screen = Screen::Viewer;
    }

    /// Viewer for the in-memory run; `agent` selects that agent's tab.
    pub fn open_run_viewer(&mut self, agent: Option<usize>, back: Screen) {
        let Some(run) = self.run.clone() else { return };
        let (q, fin, titles, reports, sources, rec) = run.with(|s| {
            (
                s.record.question.clone(),
                s.final_report.clone(),
                s.record.agents.iter().map(|a| a.title.clone()).collect::<Vec<_>>(),
                s.reports.clone(),
                s.record.sources.clone(),
                s.record.clone(),
            )
        });
        let mut tabs = vec![];
        let chat = fin.clone().map(|f| (rec.clone(), f));
        if let Some(f) = fin {
            tabs.push(Self::make_tab(final_label(&rec), f, None));
        }
        for (i, r) in reports.into_iter().enumerate() {
            if let Some(r) = r {
                tabs.push(Self::make_tab(titles[i].clone(), r, Some(i)));
            }
        }
        let sel = agent.and_then(|a| tabs.iter().position(|t| t.agent == Some(a))).unwrap_or(0);
        self.open_viewer(tabs, sel, back, q, sources, chat);
    }

    pub(super) fn open_record_viewer(&mut self, rec: &Record) {
        let mut tabs = vec![];
        if let Some(f) = rec.read_final() {
            tabs.push(Self::make_tab(final_label(rec), f, None));
        }
        for (i, a) in rec.agents.iter().enumerate() {
            if let Some(r) = rec.read_agent_report(i) {
                tabs.push(Self::make_tab(a.title.clone(), r, Some(i)));
            }
        }
        let chat = rec.read_final().map(|f| (rec.clone(), f));
        self.open_viewer(tabs, 0, Screen::History, rec.question.clone(), rec.sources.clone(), chat);
    }

    pub(super) fn open_save(&mut self) {
        let Some(t) = self.viewer.tabs.get(self.viewer.tab) else { return };
        let mut name = research::slug(&self.viewer.question);
        if t.is_sources {
            name.push_str("-sources");
        } else if let Some(i) = t.agent {
            name.push_str(&format!("-{}", i + 1));
        }
        let path = self.cwd.join(format!("{name}.md"));
        self.modal = Modal::Save(Box::new(SaveModal {
            ta: single_line_ta(&path.to_string_lossy()),
            text: t.text.clone(),
            err: None,
        }));
    }

    pub(super) fn do_save(&mut self, path: PathBuf, text: &str) {
        match std::fs::write(&path, text) {
            Ok(()) => self.toast(format!("Saved to {}", path.display())),
            Err(e) => self.toast(format!("Save failed: {e}")),
        }
    }

    /// Export prompt for the final report; defaults to HTML next to the working directory.
    pub(super) fn open_export(&mut self) {
        let Some((rec, report)) = self.viewer.chat.clone() else {
            self.toast("Export needs a final report");
            return;
        };
        let path = self.cwd.join(format!("{}.html", research::slug(&self.viewer.question)));
        self.modal = Modal::Export(Box::new(ExportModal {
            ta: single_line_ta(&path.to_string_lossy()),
            err: None,
            rec,
            report,
        }));
    }

    /// pandoc runs in the background; the result arrives as a toast.
    pub(super) fn export_pdf(&mut self, path: PathBuf, rec: Record, report: String) {
        self.toast("Exporting PDF…");
        let tx = self.run_tx.clone();
        tokio::spawn(async move {
            let msg = match export::pdf(&rec, &report, &path).await {
                Ok(()) => format!("Exported to {}", path.display()),
                Err(e) => format!("Export failed: {e}"),
            };
            let _ = tx.send(RunEvent::Toast(msg));
        });
    }

    pub(super) fn key_viewer(&mut self, k: KeyEvent) {
        if self.viewer.tabs.get(self.viewer.tab).is_some_and(|t| t.is_sources) && self.key_sources(k) {
            return;
        }
        if self.viewer.nav_key(k.code) {
            return;
        }
        match k.code {
            KeyCode::Char('s') => self.open_save(),
            KeyCode::Char('e') => self.open_export(),
            KeyCode::Char('c') => {
                if let Some(t) = self.viewer.tabs.get(self.viewer.tab) {
                    osc52_copy(&t.text);
                    let msg = format!("Copied {} to clipboard", t.label);
                    self.toast(msg);
                }
            }
            KeyCode::Char('a') => self.open_chat(),
            KeyCode::Esc => {
                let back = self.viewer.back;
                if back == Screen::History {
                    self.open_history();
                } else {
                    self.screen = back;
                }
            }
            _ => {}
        }
    }

    /// Keys specific to the Sources tab; returns false to fall through to the generic viewer keys.
    pub(super) fn key_sources(&mut self, k: KeyEvent) -> bool {
        if self.viewer.sources_nav_key(k.code) {
            return true;
        }
        let v = &self.viewer;
        match k.code {
            KeyCode::Enter | KeyCode::Char('o') => {
                if let Some(s) = v.sources.get(v.src_sel) {
                    open_url(&s.url);
                }
            }
            KeyCode::Char('c') => {
                if let Some(s) = v.sources.get(v.src_sel) {
                    osc52_copy(&s.url);
                    self.toast("URL copied");
                }
            }
            KeyCode::Char('v') => {
                if let Some(s) = v.sources.get(v.src_sel) {
                    if s.verdicts.is_empty() {
                        self.toast("No citation check for this source");
                    } else {
                        let name = if s.title.is_empty() { s.url.clone() } else { s.title.clone() };
                        let mut lines = vec![format!("[{}] {}", v.src_sel + 1, name), String::new()];
                        for d in &s.verdicts {
                            let mark = match d.verdict {
                                Support::Supported => '✓',
                                Support::Partial => '~',
                                Support::Unsupported => '✗',
                            };
                            lines.push(format!("{mark} {}", d.claim));
                            lines.push(format!("   {}", d.note));
                        }
                        self.modal = Modal::Detail(lines);
                    }
                }
            }
            _ => return false,
        }
        true
    }
}

impl Viewer {
    /// Scrolling and tab switching; false for any other key.
    pub fn nav_key(&mut self, code: KeyCode) -> bool {
        let page = self.view_h.saturating_sub(1).max(1);
        match code {
            KeyCode::Char('j') | KeyCode::Down => self.scroll = self.scroll.saturating_add(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll = self.scroll.saturating_add(page),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(page),
            KeyCode::Char('g') => self.scroll = 0,
            KeyCode::Char('G') => self.scroll = u16::MAX,
            KeyCode::Tab => {
                self.tab = (self.tab + 1) % self.tabs.len();
                self.scroll = 0;
            }
            KeyCode::BackTab => {
                self.tab = (self.tab + self.tabs.len() - 1) % self.tabs.len();
                self.scroll = 0;
            }
            KeyCode::Char(c @ '1'..='9') => {
                let p = c as usize - '1' as usize;
                if p < self.tabs.len() {
                    self.tab = p;
                    self.scroll = 0;
                }
            }
            _ => return false,
        }
        true
    }

    /// Selection movement on the Sources tab; false for any other key.
    pub fn sources_nav_key(&mut self, code: KeyCode) -> bool {
        let last = self.sources.len().saturating_sub(1);
        let page = (self.view_h as usize / 2).max(1);
        match code {
            KeyCode::Up | KeyCode::Char('k') => self.src_sel = self.src_sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.src_sel = (self.src_sel + 1).min(last),
            KeyCode::PageUp => self.src_sel = self.src_sel.saturating_sub(page),
            KeyCode::PageDown | KeyCode::Char(' ') => self.src_sel = (self.src_sel + page).min(last),
            KeyCode::Char('g') => self.src_sel = 0,
            KeyCode::Char('G') => self.src_sel = last,
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::research::store::LinkStatus;

    fn viewer(tabs: usize, sources: usize) -> Viewer {
        let tab = |i: usize| ViewTab {
            label: format!("t{i}"),
            text: String::new(),
            rendered: Text::default(),
            agent: None,
            is_sources: false,
        };
        Viewer {
            tabs: (0..tabs).map(tab).collect(),
            tab: 0,
            scroll: 7,
            back: Screen::Home,
            question: String::new(),
            view_h: 20,
            max_scroll: 0,
            sources: (0..sources)
                .map(|i| Source {
                    url: format!("https://e.com/{i}"),
                    title: String::new(),
                    agents: vec![],
                    in_final: false,
                    status: LinkStatus::Unchecked,
                    model_supplied: false,
                    verdicts: vec![],
                })
                .collect(),
            src_sel: 0,
            chat: None,
        }
    }

    #[test]
    fn tab_wraps_forward_and_resets_scroll() {
        let mut v = viewer(3, 0);
        v.tab = 2;
        assert!(v.nav_key(KeyCode::Tab));
        assert_eq!((v.tab, v.scroll), (0, 0));
    }

    #[test]
    fn backtab_wraps_to_last_tab() {
        let mut v = viewer(3, 0);
        assert!(v.nav_key(KeyCode::BackTab));
        assert_eq!((v.tab, v.scroll), (2, 0));
    }

    #[test]
    fn number_key_past_last_tab_is_a_handled_no_op() {
        let mut v = viewer(3, 0);
        assert!(v.nav_key(KeyCode::Char('9')));
        assert_eq!((v.tab, v.scroll), (0, 7));
        assert!(v.nav_key(KeyCode::Char('2')));
        assert_eq!((v.tab, v.scroll), (1, 0));
    }

    #[test]
    fn scroll_keys_and_unhandled_keys() {
        let mut v = viewer(1, 0);
        assert!(v.nav_key(KeyCode::Char('G')));
        assert_eq!(v.scroll, u16::MAX);
        assert!(v.nav_key(KeyCode::Char('g')));
        assert_eq!(v.scroll, 0);
        assert!(v.nav_key(KeyCode::PageDown));
        assert_eq!(v.scroll, 19);
        assert!(!v.nav_key(KeyCode::Char('s')));
    }

    #[test]
    fn source_selection_clamps_and_jumps() {
        let mut v = viewer(1, 3);
        for _ in 0..5 {
            assert!(v.sources_nav_key(KeyCode::Down));
        }
        assert_eq!(v.src_sel, 2);
        assert!(v.sources_nav_key(KeyCode::Char('g')));
        assert_eq!(v.src_sel, 0);
        assert!(v.sources_nav_key(KeyCode::Char('G')));
        assert_eq!(v.src_sel, 2);
        assert!(!v.sources_nav_key(KeyCode::Enter));
    }
}
