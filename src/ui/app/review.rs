use super::*;
use crate::config::MAX_AGENTS;
use crate::research::store::Perspective;

impl App {
    pub(super) fn key_review(&mut self, k: KeyEvent) {
        let Some(run) = self.run.clone() else { return };
        let n = run.review_angles().len();
        let gap = run.status() == RunStatus::GapReview;
        match k.code {
            KeyCode::Up => self.review_sel = self.review_sel.saturating_sub(1),
            KeyCode::Down => self.review_sel = (self.review_sel + 1).min(n.saturating_sub(1)),
            KeyCode::Char('e') => {
                if let Some(p) = run.review_angles().get(self.review_sel).cloned() {
                    self.modal = Modal::Angle(Box::new(AngleEdit {
                        idx: Some(self.review_sel),
                        title: single_line_ta(&p.title),
                        brief: single_line_ta(&p.brief),
                        focus_brief: false,
                    }));
                }
            }
            KeyCode::Char('a') => {
                let total = if gap { n + run.with(|s| s.record.perspectives.len()) } else { n };
                if total >= MAX_AGENTS as usize {
                    self.toast(format!("Maximum {MAX_AGENTS} angles"));
                } else {
                    self.modal = Modal::Angle(Box::new(AngleEdit {
                        idx: None,
                        title: single_line_ta(""),
                        brief: single_line_ta(""),
                        focus_brief: false,
                    }));
                }
            }
            KeyCode::Char('d') => {
                if n <= 1 {
                    self.toast(if gap {
                        "At least one angle is required — s skips"
                    } else {
                        "At least one angle is required"
                    });
                } else {
                    let mut ps = run.review_angles();
                    ps.remove(self.review_sel);
                    run.set_review_angles(ps);
                    self.review_sel = self.review_sel.min(n - 2);
                }
            }
            KeyCode::Char('p') if !gap => {
                run.replan();
                self.screen = Screen::Run;
            }
            KeyCode::Enter => {
                if gap {
                    run.approve_gaps();
                } else {
                    run.launch();
                }
                self.run_sel = 0;
                self.inspect = Inspect::default();
                self.screen = Screen::Run;
            }
            KeyCode::Char('s') if gap => {
                run.skip_gaps();
                self.screen = Screen::Run;
            }
            KeyCode::Esc if gap => self.screen = Screen::Home,
            KeyCode::Esc => {
                self.modal = Modal::Confirm(Confirm {
                    kind: ConfirmKind::CancelReview,
                    text: "Cancel this research? y/n".into(),
                });
            }
            _ => {}
        }
    }
}

/// Replace the angle at `idx` when it exists, otherwise append. Returns the new index when appended.
pub(super) fn upsert_angle(ps: &mut Vec<Perspective>, idx: Option<usize>, p: Perspective) -> Option<usize> {
    match idx {
        Some(i) if i < ps.len() => {
            ps[i] = p;
            None
        }
        _ => {
            ps.push(p);
            Some(ps.len() - 1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ps(titles: &[&str]) -> Vec<Perspective> {
        titles.iter().map(|t| Perspective { title: t.to_string(), brief: String::new() }).collect()
    }

    fn p(t: &str) -> Perspective {
        Perspective { title: t.into(), brief: "b".into() }
    }

    #[test]
    fn replaces_in_range() {
        let mut v = ps(&["a", "b"]);
        assert_eq!(upsert_angle(&mut v, Some(1), p("x")), None);
        assert_eq!(v, vec![ps(&["a"])[0].clone(), p("x")]);
    }

    #[test]
    fn appends_without_index() {
        let mut v = ps(&["a"]);
        assert_eq!(upsert_angle(&mut v, None, p("x")), Some(1));
        assert_eq!(v.len(), 2);
    }

    #[test]
    fn appends_when_index_is_out_of_range() {
        let mut v = ps(&["a"]);
        assert_eq!(upsert_angle(&mut v, Some(5), p("x")), Some(1));
        assert_eq!(v[1], p("x"));
    }
}
