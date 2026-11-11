use super::app::*;
use super::fmt::{self, SPINNER, one_line, pad, trunc};
use crate::config::MAX_AGENTS;
use crate::research::LogEntry;
use crate::research::store::{AgentRec, AgentStatus, RunStatus, progress};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Clear, Gauge, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

fn muted() -> Style {
    Style::new().fg(MUTED)
}
fn accent() -> Style {
    Style::new().fg(ACCENT)
}
fn sel_style() -> Style {
    Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
}
fn button(label: &str) -> Span<'static> {
    Span::styled(format!(" {label} "), Style::new().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD))
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let [head, body, foot] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)]).areas(area);
    draw_header(f, app, head);
    let body = body.inner(Margin::new(2, 0));
    match app.screen {
        Screen::SignIn => draw_signin(f, app, body),
        Screen::Home => draw_home(f, app, body),
        Screen::Review => draw_review(f, app, body),
        Screen::Clarify => draw_clarify(f, app, body),
        Screen::Run => draw_run(f, app, body),
        Screen::Viewer => draw_viewer(f, app, body),
        Screen::History => draw_history(f, app, body),
        Screen::Settings => draw_settings(f, app, body),
    }
    draw_footer(f, app, foot);
    draw_modal(f, app, area);
}

// ---- header / footer ----

struct RunView {
    question: String,
    status: RunStatus,
    agents: Vec<AgentRec>,
    activity: Vec<String>,
    progress: f64,
    badge: String,
    error: Option<(String, Option<String>)>,
    synth_tail: String,
    perspectives: Vec<crate::research::store::Perspective>,
    understanding: String,
    clarify_rounds: u8,
    questions: Vec<String>,
    synth_started: bool,
}

fn run_view(app: &App) -> Option<RunView> {
    let run = app.run.as_ref()?;
    Some(run.with(|s| {
        let tail_start = s.synth_text.len().saturating_sub(4000);
        let tail_start = (tail_start..=s.synth_text.len()).find(|i| s.synth_text.is_char_boundary(*i)).unwrap_or(0);
        RunView {
            question: s.record.question.clone(),
            status: s.record.status,
            agents: s.record.agents.clone(),
            activity: s.activity.clone(),
            progress: progress(&s.record, s.synth_text.len()),
            badge: fmt::badge(&s.record),
            error: s.record.error.as_ref().map(|e| (e.message.clone(), e.request_id.clone())),
            synth_tail: s.synth_text[tail_start..].to_string(),
            perspectives: s.record.perspectives.clone(),
            understanding: s.record.understanding.clone(),
            clarify_rounds: s.record.clarify_rounds,
            questions: s.record.questions.clone(),
            synth_started: !s.synth_text.is_empty() || s.record.synthesis.usage.tokens() > 0,
        }
    }))
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let w = area.width as usize;
    let run_screen = matches!(app.screen, Screen::Run | Screen::Review | Screen::Clarify);
    let rv = if run_screen { run_view(app) } else { None };
    let (left, right) = if let Some(rv) = rv {
        let right = rv.badge.clone();
        let room = w.saturating_sub(4 + right.width() + 4 + 2);
        (
            vec![
                Span::styled(" rsc", accent().add_modifier(Modifier::BOLD)),
                Span::raw("  "),
                Span::styled(trunc(&one_line(&rv.question), room), muted()),
            ],
            right,
        )
    } else {
        let mut right = String::new();
        if app.screen == Screen::Home
            && let Some(rv) = run_view(app)
            && rv.status.is_live()
        {
            right.push_str(&format!("● {} {}%   ", rv.status.label(), (rv.progress * 100.0).round() as u32));
        }
        if app.screen != Screen::SignIn
            && let Some(a) = &app.account
        {
            right.push_str(&format!("Using ChatGPT plan · {}", a.email));
        }
        (vec![Span::styled(" rsc", accent().add_modifier(Modifier::BOLD))], right)
    };
    f.render_widget(Paragraph::new(Line::from(left)), area);
    f.render_widget(Paragraph::new(Span::styled(format!("{right} "), muted())).alignment(Alignment::Right), area);
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    if let Some((t, _)) = &app.toast {
        f.render_widget(Paragraph::new(Span::styled(format!(" {t}"), accent())), area);
        return;
    }
    let hint = match app.screen {
        Screen::SignIn => "Enter continue · Esc quit".to_string(),
        Screen::Home => {
            let base = if app.home.chips {
                "←/→ move · Enter change · Tab input · ? help"
            } else {
                "Enter start · Tab options · F1 help"
            };
            if run_shown(app) { format!("{base} · Ctrl+G research") } else { base.to_string() }
        }
        Screen::Clarify => "Enter send (empty = skip) · Tab next · F1 help".to_string(),
        Screen::Review => "Enter launch · e edit · p re-plan · ? help".to_string(),
        Screen::Run => "↑/↓ agent · i inspect · Enter report · ? help".to_string(),
        Screen::Viewer if app.viewer.tabs.get(app.viewer.tab).is_some_and(|t| t.is_sources) => {
            "↑/↓ select · Enter open · c copy · Tab switch · ? help".to_string()
        }
        Screen::Viewer => "j/k scroll · Tab/1–9 switch · s save · ? help".to_string(),
        Screen::History => if app.history.filtering {
            "type to filter · Enter keep · Esc clear"
        } else {
            "Enter open · r resume · d delete · ? help"
        }
        .to_string(),
        Screen::Settings => "↑/↓ select · Enter edit · ? help".to_string(),
    };
    f.render_widget(Paragraph::new(Span::styled(format!(" {hint}"), muted())), area);
}

fn run_shown(app: &App) -> bool {
    app.run.as_ref().is_some_and(|r| r.status() != RunStatus::Cancelled)
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

// ---- screens ----

fn draw_signin(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> =
        vec![Line::from(Span::styled("rsc", accent().add_modifier(Modifier::BOLD))), Line::raw("")];
    let spin = SPINNER[app.tick % SPINNER.len()];
    match &app.signin.phase {
        SignInPhase::Idle => lines.push(Line::from(button("Continue with ChatGPT"))),
        SignInPhase::Waiting { url, .. } => {
            lines.push(Line::from(vec![Span::styled(spin, accent()), Span::raw(" Waiting for browser…")]));
            lines.push(Line::raw(""));
            if let Some(u) = url {
                lines.push(Line::from(Span::styled(u.clone(), muted())));
                lines.push(Line::raw(""));
                lines.push(Line::from(Span::styled("c copy link · Esc cancel", muted())));
            }
        }
        SignInPhase::Error(e) => {
            lines.push(Line::from(Span::styled(e.clone(), Style::new().fg(Color::Red))));
            lines.push(Line::from(Span::styled("Enter to retry", muted())));
        }
        SignInPhase::NotEnabled => {
            lines.push(Line::from("ChatGPT plan use isn't enabled"));
            lines.push(Line::from(Span::styled("Enter: enable (re-consent) · Esc: quit", muted())));
        }
    }
    let h = (lines.len() as u16 + 6).min(area.height);
    let r = centered(area, area.width.min(80), h);
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center).wrap(Wrap { trim: true }), r);
}

fn chip(label: &str, value: String, selected: bool) -> Vec<Span<'static>> {
    let vs = if selected { Style::new().fg(Color::Black).bg(ACCENT) } else { Style::new() };
    vec![Span::styled(format!("{label} "), muted()), Span::styled(value, vs), Span::raw("   ")]
}

fn draw_home(f: &mut Frame, app: &mut App, area: Rect) {
    let w = area.width.min(72);
    let total_h = 9u16.min(area.height);
    let top = area.y + area.height.saturating_sub(total_h) / 2;
    let col = Rect::new(area.x + (area.width - w) / 2, top, w, total_h);
    let [inp, _, chips, _, active, banner] = Layout::vertical([
        Constraint::Length(5),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(col);

    let focus_in = !app.home.chips;
    let border = if focus_in { accent() } else { muted() };
    app.home.input.set_block(Block::bordered().border_type(BorderType::Rounded).border_style(border));
    app.home.input.set_cursor_style(if focus_in {
        Style::new().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    });
    f.render_widget(&app.home.input, inp);

    let h = &app.home;
    let stage = |s: &crate::config::StageModel| format!("{}·{}", app.display_name(&app.resolved(s).model), s.effort);
    let sel = |i: usize| h.chips && h.chip == i;
    let mut spans: Vec<Span> = vec![];
    spans.extend(chip("Agents", h.agents.to_string(), sel(0)));
    spans.extend(chip("Planner", stage(&h.planner), sel(1)));
    spans.extend(chip("Research", stage(&h.researcher), sel(2)));
    spans.extend(chip("Synth", stage(&h.synthesizer), sel(3)));
    spans.extend(chip("Incognito", if h.incognito { "●" } else { "○" }.into(), sel(4)));
    let chips_full = Rect::new(area.x, chips.y, area.width, 1);
    f.render_widget(Paragraph::new(Line::from(spans)).alignment(Alignment::Center), chips_full);

    let msg = match (&app.cfg_err, &app.models) {
        (Some(e), _) => Some(Span::styled(e.clone(), Style::new().fg(Color::Red))),
        (_, Models::Failed(m)) => {
            Some(Span::styled(format!("Couldn't load models: {m} — r retry"), Style::new().fg(Color::Red)))
        }
        (_, Models::Loading) => {
            Some(Span::styled(format!("{} loading models…", SPINNER[app.tick % SPINNER.len()]), muted()))
        }
        _ => None,
    };
    if let Some(m) = msg {
        f.render_widget(Paragraph::new(m).alignment(Alignment::Center), banner);
    }
    if let Some(rv) = run_view(app) {
        let pct = (rv.progress * 100.0).round() as u32;
        let status = match rv.status {
            RunStatus::Planning | RunStatus::Researching | RunStatus::Synthesizing => {
                Some((format!("● {} · {pct}% — Ctrl+G to view", capitalize(rv.status.label())), accent()))
            }
            RunStatus::Clarifying => Some(("● Planner has questions — Ctrl+G to answer".to_string(), accent())),
            RunStatus::Review => Some(("● Angles ready — Ctrl+G to review".to_string(), accent())),
            RunStatus::Done => Some(("✓ Research done — Ctrl+G to read".to_string(), Style::new().fg(Color::Green))),
            RunStatus::Failed | RunStatus::Interrupted => {
                Some((format!("✗ Research {} — Ctrl+G for details", rv.status.label()), Style::new().fg(Color::Red)))
            }
            RunStatus::Cancelled => None,
        };
        if let Some((text, style)) = status {
            let line = format!("{} · {text}", trunc(&one_line(&rv.question), 30));
            f.render_widget(Paragraph::new(Span::styled(line, style)).alignment(Alignment::Center), active);
        }
    }
}

fn draw_review(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(rv) = run_view(app) else { return };
    let [stages, _, rest] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(1)]).areas(area);
    f.render_widget(Paragraph::new(stage_line(&rv)), stages);
    let w = area.width.saturating_sub(4) as usize;
    let mut lines: Vec<Line> = vec![];
    if !rv.understanding.is_empty() {
        lines.push(Line::from(Span::styled("Understood as", muted())));
        lines.extend(fmt::wrap(&rv.understanding, w).into_iter().map(Line::from));
        lines.push(Line::raw(""));
    }
    lines.push(Line::from(Span::styled(
        format!("{} angles — one researcher each. Edit freely, then Enter to launch.", rv.perspectives.len()),
        muted(),
    )));
    lines.push(Line::raw(""));
    let mut sel_start = 0;
    let mut sel_end = 0;
    for (i, p) in rv.perspectives.iter().enumerate() {
        let selected = i == app.review_sel;
        if selected {
            sel_start = lines.len();
        }
        let mark = if selected { "▸ " } else { "  " };
        lines.push(Line::from(vec![
            Span::styled(mark, accent()),
            Span::styled(
                trunc(&p.title, w),
                if selected { sel_style() } else { Style::new().add_modifier(Modifier::BOLD) },
            ),
        ]));
        for l in fmt::wrap(&p.brief, w) {
            lines.push(Line::from(Span::styled(format!("  {l}"), muted())));
        }
        if selected {
            sel_end = lines.len();
        }
        lines.push(Line::raw(""));
    }
    let h = rest.height as usize;
    let off = if sel_end > h { (sel_end - h).min(sel_start) } else { 0 };
    f.render_widget(Paragraph::new(lines).scroll((off as u16, 0)), rest);
}

fn agent_icon(a: &AgentRec, tick: usize) -> Span<'static> {
    match a.status {
        AgentStatus::Done => Span::styled("✓", Style::new().fg(Color::Green)),
        AgentStatus::Failed => Span::styled("✗", Style::new().fg(Color::Red)),
        AgentStatus::Running => Span::styled(SPINNER[tick % SPINNER.len()], accent()),
        AgentStatus::Pending => Span::styled("·", muted()),
    }
}

fn draw_run(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(rv) = run_view(app) else {
        f.render_widget(Paragraph::new(Span::styled("No research in progress", muted())), area);
        return;
    };
    let synthesizing = rv.status == RunStatus::Synthesizing;
    let err_h =
        if matches!(rv.status, RunStatus::Failed | RunStatus::Cancelled | RunStatus::Interrupted) { 2 } else { 0 };
    let [stages, gauge, _, body, synth, errs] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(if synthesizing { 9 } else { 0 }),
        Constraint::Length(err_h),
    ])
    .areas(area);

    f.render_widget(Paragraph::new(stage_line(&rv)), stages);

    let pct = (rv.progress * 100.0).round() as u32;
    let done = rv.agents.iter().filter(|a| a.status == AgentStatus::Done).count();
    let label = match rv.status {
        RunStatus::Planning => format!("Planning · {pct}%"),
        RunStatus::Researching => format!("Researching · {done}/{} done · {pct}%", rv.agents.len()),
        RunStatus::Synthesizing => format!("Synthesizing · {pct}%"),
        s => format!("{} · {pct}%", s.label()),
    };
    f.render_widget(
        Gauge::default()
            .ratio(rv.progress.clamp(0.0, 1.0))
            .label(label)
            .gauge_style(Style::new().fg(ACCENT).bg(Color::Black))
            .use_unicode(true),
        gauge,
    );

    let n = rv.agents.len();
    if n == 0 {
        draw_timeline(f, app, body, " Planner ".into(), None, None, "Starting…");
    } else {
        let sel = app.run_sel.min(n - 1);
        let a = &rv.agents[sel];
        let state = match a.status {
            AgentStatus::Pending => "waiting",
            AgentStatus::Running => "working",
            AgentStatus::Done => "done",
            AgentStatus::Failed => "failed",
        };
        let title = format!(" {} {} · {state} ", sel + 1, a.title);
        let empty = if a.status == AgentStatus::Pending { "Waiting for a free slot" } else { "No activity yet" };
        if app.inspect.full {
            draw_timeline(f, app, body, title, Some(a.brief.clone()), Some(sel), empty);
        } else if body.width >= 96 {
            let lw = (body.width * 2 / 5).clamp(32, 56);
            let [left, _, right] =
                Layout::horizontal([Constraint::Length(lw), Constraint::Length(1), Constraint::Min(1)]).areas(body);
            draw_agent_list(f, app, &rv, left, true);
            draw_timeline(f, app, right, title, Some(a.brief.clone()), Some(sel), empty);
        } else {
            draw_agent_list(f, app, &rv, body, false);
        }
    }

    if synthesizing {
        let p = Paragraph::new(rv.synth_tail.clone()).style(muted()).wrap(Wrap { trim: false });
        let lines = p.line_count(synth.width);
        let h = synth.height.saturating_sub(1);
        f.render_widget(
            Paragraph::new(Span::styled("synthesis", muted())),
            Rect::new(synth.x, synth.y, synth.width, 1),
        );
        let body = Rect::new(synth.x, synth.y + 1, synth.width, h);
        f.render_widget(p.scroll((lines.saturating_sub(h as usize) as u16, 0)), body);
    }

    if err_h > 0 {
        let red = Style::new().fg(Color::Red);
        let mut l1 = match &rv.error {
            Some((m, rid)) => {
                let mut t = m.clone();
                if let Some(r) = rid {
                    t.push_str(&format!(" (request {r})"));
                }
                t
            }
            None => rv.status.label().to_string(),
        };
        l1 = trunc(&one_line(&l1), errs.width as usize);
        let st = if rv.status == RunStatus::Failed { red } else { muted() };
        f.render_widget(
            Paragraph::new(vec![Line::from(Span::styled(l1, st)), Line::from(Span::styled("r resume", muted()))]),
            errs,
        );
    }
}

fn draw_viewer(f: &mut Frame, app: &mut App, area: Rect) {
    let [tabs_area, _, body] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(1)]).areas(area);
    let v = &mut app.viewer;
    let labels: Vec<String> = v
        .tabs
        .iter()
        .enumerate()
        .map(|(i, t)| if i < 9 { format!("{} {}", i + 1, trunc(&t.label, 22)) } else { trunc(&t.label, 22) })
        .collect();
    let mut start = 0;
    while start < v.tab && labels[start..=v.tab].iter().map(|l| l.width() + 3).sum::<usize>() > tabs_area.width as usize
    {
        start += 1;
    }
    let mut spans = vec![];
    for (i, l) in labels.iter().enumerate().skip(start) {
        let st = if i == v.tab { sel_style().add_modifier(Modifier::UNDERLINED) } else { muted() };
        spans.push(Span::styled(l.clone(), st));
        spans.push(Span::raw("   "));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), tabs_area);

    let Some(t) = v.tabs.get(v.tab) else { return };
    if t.is_sources {
        draw_sources(f, v, body);
        return;
    }
    let p = Paragraph::new(t.rendered.clone()).wrap(Wrap { trim: false });
    let lines = p.line_count(body.width);
    v.view_h = body.height;
    v.max_scroll = lines.saturating_sub(body.height as usize).min(u16::MAX as usize) as u16;
    v.scroll = v.scroll.min(v.max_scroll);
    f.render_widget(p.scroll((v.scroll, 0)), body);
}

fn draw_sources(f: &mut Frame, v: &mut Viewer, area: Rect) {
    let [head, list] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
    let n = v.sources.len();
    let cited = v.sources.iter().filter(|s| s.in_final).count();
    f.render_widget(
        Paragraph::new(Span::styled(format!("{n} sources · {cited} cited in the final report"), muted())),
        head,
    );

    v.view_h = list.height;
    let per = (list.height as usize / 2).max(1);
    v.src_sel = v.src_sel.min(n.saturating_sub(1));
    let off = if v.src_sel >= per { v.src_sel + 1 - per } else { 0 };
    let width = list.width as usize;
    for (row, i) in (off..n.min(off + per)).enumerate() {
        let s = &v.sources[i];
        let y = list.y + 2 * row as u16;
        if y >= list.y + list.height {
            break;
        }
        let selected = i == v.src_sel;
        let marker = if selected { "▸ " } else { "  " };
        let num = format!("[{}] ", i + 1);
        let mut tag = s.agents.iter().map(|a| format!("#{}", a + 1)).collect::<Vec<_>>().join(" ");
        if s.in_final {
            tag.push_str(if tag.is_empty() { "final" } else { " · final" });
        }
        let title = if s.title.is_empty() { crate::research::tools::host_of(&s.url) } else { one_line(&s.title) };
        let room = width.saturating_sub(marker.width() + num.width() + tag.width() + 1);
        let title_style = if selected { sel_style() } else { Style::new() };
        let r1 = Rect::new(list.x, y, list.width, 1);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(marker, accent()),
                Span::styled(num, accent()),
                Span::styled(trunc(&title, room), title_style),
            ])),
            r1,
        );
        f.render_widget(Paragraph::new(Span::styled(tag, muted())).alignment(Alignment::Right), r1);
        if y + 1 < list.y + list.height {
            let r2 = Rect::new(list.x, y + 1, list.width, 1);
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::raw("      "),
                    Span::styled(trunc(&s.url, width.saturating_sub(6)), muted()),
                ])),
                r2,
            );
        }
    }
}

fn draw_history(f: &mut Frame, app: &mut App, area: Rect) {
    let [filter, list] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
    if app.history.filtering || !app.history.filter.is_empty() {
        let cursor = if app.history.filtering { "▏" } else { "" };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("/ ", muted()),
                Span::raw(format!("{}{cursor}", app.history.filter)),
            ])),
            filter,
        );
    }
    let items = app.history_visible();
    if items.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled("No research yet", muted())).alignment(Alignment::Center),
            centered(list, list.width, 1),
        );
        return;
    }
    let h = list.height as usize;
    let off = if app.history.sel >= h { app.history.sel + 1 - h } else { 0 };
    let qw = (list.width as usize).saturating_sub(2 + 16 + 2 + 12 + 2 + 9);
    for (row, (i, r)) in items.iter().enumerate().skip(off).take(h).map(|(i, r)| (i, *r)).enumerate() {
        let selected = i == app.history.sel;
        let date = chrono::DateTime::parse_from_rfc3339(&r.created_at)
            .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default();
        let status_style = if r.status == RunStatus::Failed { Style::new().fg(Color::Red) } else { muted() };
        let rect = Rect::new(list.x, list.y + row as u16, list.width, 1);
        let line = Line::from(vec![
            Span::styled(if selected { "▸ " } else { "  " }, accent()),
            Span::styled(pad(&date, 16), muted()),
            Span::raw("  "),
            Span::styled(
                pad(
                    &if r.config.incognito {
                        format!("[incognito] {}", one_line(&r.question))
                    } else {
                        one_line(&r.question)
                    },
                    qw,
                ),
                if selected { sel_style() } else { Style::new() },
            ),
            Span::raw("  "),
            Span::styled(pad(r.status.label(), 12), status_style),
        ]);
        f.render_widget(Paragraph::new(line), rect);
        let cost = format!("{}{}", fmt::money(r.cost_usd_total), if r.cost_partial { "+" } else { "" });
        f.render_widget(Paragraph::new(Span::styled(cost, muted())).alignment(Alignment::Right), rect);
    }
}

fn draw_settings(f: &mut Frame, app: &mut App, area: Rect) {
    let lw = 26;
    for (i, row) in ROWS.iter().enumerate() {
        if i as u16 >= area.height {
            break;
        }
        let (label, value, hint): (String, String, &str) = match row {
            Row::Account => {
                ("Account".into(), app.account.as_ref().map(|a| a.email.clone()).unwrap_or_else(|| "—".into()), "")
            }
            Row::Usage => ("Manage usage ↗".into(), String::new(), ""),
            Row::Model(st) => {
                let sm = app.current_stage(PickScope::Settings, *st);
                (st.label().into(), format!("{}·{}", app.display_name(&sm.model), sm.effort), "")
            }
            Row::Num(Num::DefaultAgents) => ("Default agents".into(), app.cfg.default_agents.to_string(), ""),
            Row::Num(Num::MaxParallel) => ("Max parallel agents".into(), app.cfg.max_parallel.to_string(), ""),
            Row::Num(Num::ReportWords) => {
                let v = app.cfg.report_words;
                ("Report length (words)".into(), if v == 0 { "auto".to_string() } else { v.to_string() }, "")
            }
            Row::FetchUrl => ("fetch_url tool".into(), on_off(app.cfg.fetch_url_enabled), "sites see your IP"),
            Row::Incognito => ("Incognito by default".into(), on_off(app.cfg.incognito_default), ""),
        };
        let selected = i == app.settings_sel;
        let line = Line::from(vec![
            Span::styled(if selected { "▸ " } else { "  " }, accent()),
            Span::styled(pad(&label, lw), if selected { sel_style() } else { Style::new() }),
            Span::raw(value),
            Span::styled(if hint.is_empty() { String::new() } else { format!("   {hint}") }, muted()),
        ]);
        f.render_widget(Paragraph::new(line), Rect::new(area.x, area.y + i as u16, area.width, 1));
    }
}

fn on_off(b: bool) -> String {
    if b { "on" } else { "off" }.into()
}

// ---- modals ----

fn modal_block(title: &str) -> Block<'static> {
    let mut b = Block::bordered().border_type(BorderType::Rounded).border_style(accent());
    if !title.is_empty() {
        b = b.title(Span::styled(format!(" {title} "), accent()));
    }
    b
}

fn draw_modal(f: &mut Frame, app: &mut App, area: Rect) {
    match &mut app.modal {
        Modal::None => {}
        Modal::Welcome => {
            let r = centered(area, 56, 8);
            f.render_widget(Clear, r);
            let text = vec![
                Line::from(Span::styled("You're using your ChatGPT plan", Style::new().add_modifier(Modifier::BOLD))),
                Line::raw(""),
                Line::from(Span::styled(
                    "Research requests use your plan's usage. Manage it anytime in ChatGPT settings.",
                    muted(),
                )),
                Line::raw(""),
                Line::from(button("Got it")),
            ];
            f.render_widget(
                Paragraph::new(text)
                    .alignment(Alignment::Center)
                    .wrap(Wrap { trim: true })
                    .block(modal_block("").padding(ratatui::widgets::Padding::horizontal(1))),
                r,
            );
        }
        Modal::UsageLimit => {
            let r = centered(area, 56, 8);
            f.render_widget(Clear, r);
            let text = vec![
                Line::from(Span::styled("Usage limit reached", Style::new().add_modifier(Modifier::BOLD))),
                Line::raw(""),
                Line::from(Span::styled("Your ChatGPT plan or rsc's app limit was reached.", muted())),
                Line::raw(""),
                Line::from(vec![button("Manage usage"), Span::styled("   Esc Close", muted())]),
            ];
            f.render_widget(
                Paragraph::new(text)
                    .alignment(Alignment::Center)
                    .wrap(Wrap { trim: true })
                    .block(modal_block("").padding(ratatui::widgets::Padding::horizontal(1))),
                r,
            );
        }
        Modal::Detail(lines) => {
            let h = (lines.len() as u16 + 2).max(5);
            let r = centered(area, 70, h);
            f.render_widget(Clear, r);
            let text: Vec<Line> = lines.iter().map(|l| Line::from(l.clone())).collect();
            f.render_widget(Paragraph::new(text).wrap(Wrap { trim: true }).block(modal_block("")), r);
        }
        Modal::Confirm(c) => {
            let r = centered(area, (c.text.width() as u16 + 6).max(30), 3);
            f.render_widget(Clear, r);
            f.render_widget(Paragraph::new(c.text.clone()).alignment(Alignment::Center).block(modal_block("")), r);
        }
        Modal::Picker(_) => draw_picker(f, app, area),
        Modal::Angle(a) => {
            let r = centered(area, 70.min(area.width), 9);
            f.render_widget(Clear, r);
            f.render_widget(modal_block(if a.idx.is_some() { "Edit angle" } else { "Add angle" }), r);
            let inner = r.inner(Margin::new(1, 1));
            let [t, b, hint] =
                Layout::vertical([Constraint::Length(3), Constraint::Length(3), Constraint::Length(1)]).areas(inner);
            for (ta, label, focused, rect) in
                [(&mut a.title, "Title", !a.focus_brief, t), (&mut a.brief, "Brief", a.focus_brief, b)]
            {
                ta.set_block(
                    Block::bordered()
                        .border_type(BorderType::Rounded)
                        .border_style(if focused { accent() } else { muted() })
                        .title(label),
                );
                ta.set_cursor_style(if focused {
                    Style::new().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                });
                f.render_widget(&*ta, rect);
            }
            f.render_widget(Paragraph::new(Span::styled("Tab switch · Enter save · Esc cancel", muted())), hint);
        }
        Modal::Save(s) => {
            let r = centered(area, 80.min(area.width), 6);
            f.render_widget(Clear, r);
            f.render_widget(modal_block("Save as"), r);
            let inner = r.inner(Margin::new(1, 1));
            let [t, e] = Layout::vertical([Constraint::Length(3), Constraint::Length(1)]).areas(inner);
            s.ta.set_block(Block::bordered().border_type(BorderType::Rounded).border_style(muted()));
            s.ta.set_cursor_style(Style::new().add_modifier(Modifier::REVERSED));
            f.render_widget(&s.ta, t);
            if let Some(err) = &s.err {
                f.render_widget(Paragraph::new(Span::styled(err.clone(), Style::new().fg(Color::Red))), e);
            }
        }
        Modal::Number(n) => {
            let title = match n.which {
                Num::DefaultAgents => format!("Default agents (1–{MAX_AGENTS})"),
                Num::MaxParallel => format!("Max parallel agents (1–{MAX_AGENTS})"),
                Num::ReportWords => "Report words (0 = auto)".to_string(),
            };
            let r = centered(area, 44.min(area.width), 6);
            f.render_widget(Clear, r);
            f.render_widget(modal_block(&title), r);
            let inner = r.inner(Margin::new(1, 1));
            let [t, e] = Layout::vertical([Constraint::Length(3), Constraint::Length(1)]).areas(inner);
            n.ta.set_block(Block::bordered().border_type(BorderType::Rounded).border_style(muted()));
            n.ta.set_cursor_style(Style::new().add_modifier(Modifier::REVERSED));
            f.render_widget(&n.ta, t);
            if let Some(err) = &n.err {
                f.render_widget(Paragraph::new(Span::styled(err.clone(), Style::new().fg(Color::Red))), e);
            }
        }
        Modal::Account(items, sel) => {
            let h = items.len() as u16 + 2;
            let r = centered(area, 56.min(area.width), h);
            f.render_widget(Clear, r);
            let lines: Vec<Line> = items
                .iter()
                .enumerate()
                .map(|(i, (l, _))| {
                    if i == *sel {
                        Line::from(vec![Span::styled("▸ ", accent()), Span::styled(l.clone(), sel_style())])
                    } else {
                        Line::from(format!("  {l}"))
                    }
                })
                .collect();
            f.render_widget(Paragraph::new(lines).block(modal_block("Account")), r);
        }
        Modal::Help => draw_help(f, app, area),
    }
}

fn draw_picker(f: &mut Frame, app: &App, area: Rect) {
    let Modal::Picker(p) = &app.modal else { return };
    let title =
        if p.effort_step { format!("{} · effort", p.stage.label()) } else { format!("{} · model", p.stage.label()) };
    let lines: Vec<Line> = match &app.models {
        Models::Ready(_) => {
            let entries = app.picker_entries(p);
            let h = 12usize;
            let off = if p.sel >= h { p.sel + 1 - h } else { 0 };
            entries
                .iter()
                .enumerate()
                .skip(off)
                .take(h)
                .map(|(i, (l, cur))| {
                    let mark = if *cur { " ✓" } else { "" };
                    if i == p.sel {
                        Line::from(vec![Span::styled("▸ ", accent()), Span::styled(format!("{l}{mark}"), sel_style())])
                    } else {
                        Line::from(format!("  {l}{mark}"))
                    }
                })
                .collect()
        }
        Models::Loading => vec![Line::from(Span::styled("Loading models…", muted()))],
        Models::Failed(m) => vec![
            Line::from(Span::styled(format!("Couldn't load models: {m}"), Style::new().fg(Color::Red))),
            Line::from(Span::styled("r retry · Esc close", muted())),
        ],
    };
    let h = (lines.len() as u16 + 2).max(4);
    let r = centered(area, 56.min(area.width), h);
    f.render_widget(Clear, r);
    f.render_widget(Paragraph::new(Text::from(lines)).wrap(Wrap { trim: true }).block(modal_block(&title)), r);
}

// ---- help ----

fn help_entries(app: &App) -> Vec<(&'static str, &'static str)> {
    match app.screen {
        Screen::Home if app.home.chips => vec![
            ("←/→", "move between options"),
            ("Enter/Space", "change option"),
            ("+/-", "agents"),
            ("m", "manage usage"),
            ("r", "retry loading models"),
            ("Tab/Esc", "back to input"),
        ],
        Screen::Home => vec![("Enter", "start research"), ("Alt+Enter", "new line"), ("Tab", "options row")],
        Screen::Clarify => vec![
            ("Enter", "send answers"),
            ("Enter (all empty)", "skip, plan now"),
            ("Tab/Shift+Tab", "next/previous answer"),
            ("Alt+Enter", "new line"),
            ("Esc", "cancel research"),
        ],
        Screen::Review => vec![
            ("↑/↓", "select angle"),
            ("e", "edit"),
            ("a", "add"),
            ("d", "delete"),
            ("p", "re-plan the angles"),
            ("Enter", "launch"),
            ("Esc", "cancel research"),
        ],
        Screen::Run => vec![
            ("↑/↓", "select agent"),
            ("i", "full-screen inspector"),
            ("PgUp/PgDn", "scroll timeline"),
            ("End", "follow latest"),
            ("Enter", "open report"),
            ("x", "cancel"),
            ("r", "resume"),
            ("Esc", "home"),
        ],
        Screen::Viewer => vec![
            ("j/k ↑/↓", "scroll"),
            ("Space/PgDn, PgUp", "page"),
            ("g/G", "top/bottom"),
            ("Tab/Shift+Tab", "switch tab"),
            ("1–9", "jump to tab"),
            ("s", "save"),
            ("Enter/o", "open source (Sources tab)"),
            ("c", "copy source URL (Sources tab)"),
            ("Esc", "back"),
        ],
        Screen::History => vec![
            ("↑/↓", "select"),
            ("Enter", "open"),
            ("/", "filter"),
            ("r", "resume"),
            ("d", "delete"),
            ("Esc", "home"),
        ],
        Screen::Settings => vec![("↑/↓", "select"), ("Enter/Space", "edit or toggle"), ("Esc", "back")],
        Screen::SignIn => vec![],
    }
}

const GLOBAL_KEYS: [(&str, &str); 6] = [
    ("Ctrl+G", "current research"),
    ("Ctrl+R", "history"),
    ("Ctrl+O", "settings"),
    ("Ctrl+U", "manage usage"),
    ("F1", "help"),
    ("Ctrl+C", "quit"),
];

fn draw_help(f: &mut Frame, app: &App, area: Rect) {
    let row = |(k, d): (&str, &str)| Line::from(vec![Span::styled(pad(k, 16), accent()), Span::raw(d.to_string())]);
    let mut lines: Vec<Line> = help_entries(app).into_iter().map(row).collect();
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled("Anywhere", muted())));
    lines.extend(GLOBAL_KEYS.into_iter().map(row));
    let r = centered(area, 64, lines.len() as u16 + 2);
    f.render_widget(Clear, r);
    f.render_widget(Paragraph::new(lines).block(modal_block("Keys")), r);
}

// ---- run timelines ----

fn entry_lines(e: &LogEntry, width: usize) -> Vec<Line<'static>> {
    let tw = width.saturating_sub(2).max(1);
    match e {
        LogEntry::Thinking(t) => {
            let style = muted().add_modifier(Modifier::ITALIC);
            let mut out: Vec<Line<'static>> = vec![];
            for part in t.split('\n') {
                for l in fmt::wrap(part, tw) {
                    let gutter = if out.is_empty() { "∴ " } else { "  " };
                    out.push(Line::from(vec![Span::styled(gutter, muted()), Span::styled(l, style)]));
                }
            }
            out
        }
        LogEntry::Search(q) => vec![Line::from(vec![
            Span::styled("⌕ ", accent()),
            Span::styled("search ", muted()),
            Span::raw(trunc(q, width.saturating_sub(9))),
        ])],
        LogEntry::Open(u) => vec![Line::from(vec![
            Span::styled("↗ ", accent()),
            Span::styled("open ", muted()),
            Span::raw(trunc(u, width.saturating_sub(7))),
        ])],
        LogEntry::Text(t) => {
            let mut out: Vec<Line<'static>> = vec![];
            if t.trim().is_empty() {
                return out;
            }
            for part in t.split('\n') {
                if part.trim().is_empty() {
                    out.push(Line::from(Span::styled("│ ", muted())));
                    continue;
                }
                for l in fmt::wrap(part, tw) {
                    out.push(Line::from(vec![Span::styled("│ ", muted()), Span::raw(l)]));
                }
            }
            out
        }
        LogEntry::Note(n) => {
            vec![Line::from(Span::styled(format!("— {}", trunc(n, width.saturating_sub(2))), muted()))]
        }
    }
}

/// Last lines of a timeline, oldest first. Walks entries newest-first and stops once `want` lines are
/// collected, so per-frame work is bounded by the viewport. The flag is true when older entries remain.
fn timeline_tail(log: &[LogEntry], width: usize, want: usize) -> (Vec<Line<'static>>, bool) {
    let mut chunks: Vec<Vec<Line<'static>>> = vec![];
    let mut n = 0;
    for e in log.iter().rev() {
        if n >= want {
            break;
        }
        let l = entry_lines(e, width);
        n += l.len();
        chunks.push(l);
    }
    let more = chunks.len() < log.len();
    (chunks.into_iter().rev().flatten().collect(), more)
}

/// Bordered timeline panel. `src` is `None` for the planner, `Some(i)` for agent `i`.
fn draw_timeline(
    f: &mut Frame,
    app: &mut App,
    area: Rect,
    title: String,
    brief: Option<String>,
    src: Option<usize>,
    empty: &str,
) {
    let inner = Block::bordered().inner(area);
    let w = inner.width as usize;
    let brief_lines: Vec<String> = brief.map(|b| fmt::wrap(&b, w).into_iter().take(2).collect()).unwrap_or_default();
    let bh = if brief_lines.is_empty() { 0 } else { (brief_lines.len() as u16 + 1).min(inner.height) };
    let tl = Rect::new(inner.x, inner.y + bh, inner.width, inner.height - bh);
    let h = tl.height as usize;
    app.inspect.view_h = h;
    let want = h + app.inspect.scroll;
    let (lines, more) = match &app.run {
        Some(run) => run.with(|s| {
            let log: &[LogEntry] = match src {
                None => &s.planner_log,
                Some(i) => s.logs.get(i).map(|v| v.as_slice()).unwrap_or(&[]),
            };
            timeline_tail(log, w, want)
        }),
        None => (vec![], false),
    };
    let max = if more { app.inspect.scroll } else { lines.len().saturating_sub(h) };
    app.inspect.scroll = app.inspect.scroll.min(max);
    let scroll = app.inspect.scroll;

    let mut block =
        Block::bordered().border_type(BorderType::Rounded).border_style(muted()).title(Span::styled(title, muted()));
    if scroll > 0 {
        block = block.title_bottom(Span::styled(" End ↓ latest ", muted()));
    }
    f.render_widget(block, area);
    if bh > 0 {
        let bl: Vec<Line> = brief_lines.into_iter().map(|l| Line::from(Span::styled(l, muted()))).collect();
        f.render_widget(Paragraph::new(bl), Rect::new(inner.x, inner.y, inner.width, bh));
    }
    if lines.is_empty() {
        f.render_widget(Paragraph::new(Span::styled(empty.to_string(), muted())), tl);
        return;
    }
    let end = lines.len() - scroll;
    let visible: Vec<Line<'static>> = lines[end.saturating_sub(h)..end].to_vec();
    f.render_widget(Paragraph::new(visible), tl);
}

fn draw_agent_list(f: &mut Frame, app: &App, rv: &RunView, list: Rect, compact: bool) {
    let cfg = app.run.as_ref().map(|r| r.with(|s| s.record.config.clone()));
    let model_tag =
        cfg.map(|c| format!("{}·{}", app.display_name(&c.researcher.model), c.researcher.effort)).unwrap_or_default();
    let usage_by_agent: Vec<String> = rv
        .agents
        .iter()
        .map(|a| match a.cost_usd {
            Some(c) if a.usage.tokens() > 0 => fmt::money(c),
            _ if a.usage.tokens() > 0 => fmt::tokens(a.usage.tokens()),
            _ => String::new(),
        })
        .collect();
    let n = rv.agents.len();
    let h = list.height as usize;
    let off = if app.run_sel >= h { app.run_sel + 1 - h } else { 0 };
    let tw = (list.width as usize / 4).clamp(10, 28);
    let mw = model_tag.width().min(24);
    for (row, i) in (off..n.min(off + h)).enumerate() {
        let a = &rv.agents[i];
        let r = Rect::new(list.x, list.y + row as u16, list.width, 1);
        let cost = usage_by_agent.get(i).cloned().unwrap_or_default();
        let selected = i == app.run_sel;
        let title_style = if selected { sel_style() } else { Style::new() };
        let line = if compact {
            let ctw = (list.width as usize).saturating_sub(2 + cost.width() + 1);
            Line::from(vec![agent_icon(a, app.tick), Span::raw(" "), Span::styled(pad(&a.title, ctw), title_style)])
        } else {
            let act = if a.status == AgentStatus::Failed {
                a.error.clone().unwrap_or_default()
            } else {
                rv.activity.get(i).cloned().unwrap_or_default()
            };
            let fixed = 2 + tw + 1 + mw + 2;
            let room = (list.width as usize).saturating_sub(fixed + cost.width() + 2);
            Line::from(vec![
                agent_icon(a, app.tick),
                Span::raw(" "),
                Span::styled(pad(&a.title, tw), title_style),
                Span::raw(" "),
                Span::styled(pad(&model_tag, mw), muted()),
                Span::raw("  "),
                Span::styled(
                    trunc(&one_line(&act), room),
                    if a.status == AgentStatus::Failed { Style::new().fg(Color::Red) } else { muted() },
                ),
            ])
        };
        f.render_widget(Paragraph::new(line), r);
        f.render_widget(Paragraph::new(Span::styled(cost, muted())).alignment(Alignment::Right), r);
    }
}

fn stage_line(rv: &RunView) -> Line<'static> {
    let mut names = vec!["Plan"];
    if rv.status == RunStatus::Clarifying || rv.clarify_rounds > 0 {
        names.push("Questions");
    }
    names.extend(["Review", "Research", "Synthesize"]);
    let current = match rv.status {
        RunStatus::Planning => Some(if rv.clarify_rounds > 0 { "Questions" } else { "Plan" }),
        RunStatus::Clarifying => Some("Questions"),
        RunStatus::Review => Some("Review"),
        RunStatus::Researching => Some("Research"),
        RunStatus::Synthesizing => Some("Synthesize"),
        RunStatus::Done => None,
        RunStatus::Failed | RunStatus::Cancelled | RunStatus::Interrupted => Some(if rv.agents.is_empty() {
            "Plan"
        } else if rv.synth_started {
            "Synthesize"
        } else {
            "Research"
        }),
    };
    let cur = match current {
        Some(c) => names.iter().position(|n| *n == c).unwrap_or(0),
        None => names.len(),
    };
    let mut spans: Vec<Span<'static>> = vec![];
    for (i, n) in names.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ›  ", muted()));
        }
        spans.push(if i < cur {
            Span::styled(format!("✓ {n}"), muted())
        } else if i == cur {
            match rv.status {
                RunStatus::Failed => {
                    Span::styled(format!("✗ {n}"), Style::new().fg(Color::Red).add_modifier(Modifier::BOLD))
                }
                RunStatus::Cancelled | RunStatus::Interrupted => Span::styled(format!("■ {n}"), muted()),
                _ => Span::styled(format!("● {n}"), sel_style()),
            }
        } else {
            Span::styled(n.to_string(), muted())
        });
    }
    Line::from(spans)
}

fn draw_clarify(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(rv) = run_view(app) else { return };
    let w = area.width as usize;
    let mut und: Vec<Line> = vec![];
    if !rv.understanding.is_empty() {
        und.push(Line::from(Span::styled("Understood as", muted())));
        und.extend(fmt::wrap(&rv.understanding, w).into_iter().take(4).map(Line::from));
    }
    let q_lines: Vec<Vec<String>> =
        rv.questions.iter().enumerate().map(|(i, q)| fmt::wrap(&format!("{}. {q}", i + 1), w)).collect();
    let mut cons =
        vec![Constraint::Length(1), Constraint::Length(1), Constraint::Length(und.len() as u16), Constraint::Length(1)];
    for ql in &q_lines {
        cons.extend([Constraint::Length(ql.len() as u16), Constraint::Length(3), Constraint::Length(1)]);
    }
    let rects = Layout::vertical(cons).split(area);
    f.render_widget(Paragraph::new(stage_line(&rv)), rects[0]);
    f.render_widget(Paragraph::new(und), rects[2]);
    let focus = app.clarify.focus;
    for (i, ql) in q_lines.into_iter().enumerate() {
        let qs: Vec<Line> =
            ql.into_iter().map(|l| Line::from(Span::styled(l, Style::new().add_modifier(Modifier::BOLD)))).collect();
        f.render_widget(Paragraph::new(qs), rects[4 + 3 * i]);
        let Some(ta) = app.clarify.answers.get_mut(i) else { continue };
        let focused = i == focus;
        ta.set_block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(if focused { accent() } else { muted() })
                .title(" Answer "),
        );
        ta.set_cursor_style(if focused { Style::new().add_modifier(Modifier::REVERSED) } else { Style::default() });
        f.render_widget(&*ta, rects[5 + 3 * i]);
    }
}
