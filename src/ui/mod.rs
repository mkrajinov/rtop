//! Rendering. Pure functions of `App` state.

mod detail;
mod explain;
pub mod header;
mod lab;
#[cfg(feature = "kube")]
pub mod picker;
mod table;
pub mod theme;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Padding, Paragraph, Wrap};

use crate::app::{App, Mode, SIGNALS};
use crate::model::Target;

/// Catalog keys the UI refers to outside of `Field`s (checked by a test).
pub const EXPLAIN_KEYS: &[&str] = &["maps", "fds", "threads", "environ", "raw", "limits", "namespaces", "pid"];

pub fn draw(f: &mut Frame, app: &mut App) {
    // A one-column gutter on both sides so nothing touches the window edge.
    let full = f.area();
    let area = Rect { x: full.x + 1, width: full.width.saturating_sub(2), ..full };
    let lab_height = if app.lab.is_some() { 9 } else { 0 };
    // Short terminals give every row to the process list.
    let dashboard = if app.mode == Mode::Detail { 0 } else { header::height(area.height) };
    let header_height = dashboard + 1;
    let chunks = Layout::vertical([
        Constraint::Length(header_height),
        Constraint::Min(5),
        Constraint::Length(lab_height),
        Constraint::Length(1),
    ])
    .split(area);

    if app.collector.target().is_some() {
        header::draw_target(f, app, Rect { height: 1, ..chunks[0] });
    }
    if header_height > 1 {
        header::draw(f, app, Rect { y: chunks[0].y + 1, height: chunks[0].height - 1, ..chunks[0] });
    }

    let (main, side) = if app.show_explain {
        let c = Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)]).split(chunks[1]);
        (c[0], Some(c[1]))
    } else {
        (chunks[1], None)
    };
    match (&app.mode, app.detail.is_some()) {
        (_, true) => detail::draw(f, app, main),
        _ => table::draw_table(f, app, main),
    }
    if let Some(side) = side {
        explain::draw(f, app, side);
    }
    if app.lab.is_some() {
        lab::draw(f, app, chunks[2]);
    }
    draw_footer(f, app, chunks[3]);

    match app.mode {
        Mode::Signal => draw_signals(f, app, full),
        Mode::Help => draw_help(f, app, full),
        Mode::Theme => draw_theme(f, &app.theme, full),
        _ => {}
    }
    theme::finalize(f.buffer_mut());
}

pub fn panel(title: impl Into<String>) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::pal().faint))
        .padding(Padding::horizontal(1))
        .title(Span::styled(format!(" {} ", title.into()), theme::title()))
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let line = if let Some(msg) = app.status_text() {
        Line::from(Span::styled(format!(" {msg}"), Style::default().fg(theme::pal().warn)))
    } else if app.mode == Mode::Filter {
        Line::from(vec![
            Span::styled(" filter: ", theme::title()),
            Span::raw(app.filter.clone()),
            Span::styled("▏", Style::default().fg(theme::pal().accent)),
            Span::styled("   Enter keep · Esc clear", theme::dim()),
        ])
    } else {
        let keys: &[(&str, &str)] = if app.detail.is_some() {
            &[
                ("←→/1-8", "tab"),
                ("↑↓", "select"),
                ("e", "explain"),
                ("r", "raw source"),
                ("[ ]", "scroll explain"),
                ("Esc", "back"),
                ("T", "theme"),
                ("?", "help"),
            ]
        } else if let Some(Target::Node { .. }) = app.collector.target() {
            &[
                ("↑↓", "select"),
                ("←→", "column"),
                ("s", "sort"),
                ("e", "explain"),
                ("Enter", "inspect"),
                ("Esc", "pods"),
                ("/", "filter pod or command"),
                ("t", "tree"),
                ("f", "family"),
                ("T", "theme"),
                ("D", "graphs"),
                ("?", "help"),
                ("q", "quit"),
            ]
        } else if app.collector.target().is_some() {
            &[
                ("↑↓", "select"),
                ("←→", "column"),
                ("s", "sort"),
                ("e", "explain"),
                ("Enter", "inspect"),
                ("Esc", "pods"),
                ("L", "lessons"),
                ("/", "filter"),
                ("C", "container"),
                ("t", "tree"),
                ("f", "family"),
                ("k", "signal"),
                ("T", "theme"),
                ("D", "graphs"),
                ("?", "help"),
                ("q", "quit"),
            ]
        } else {
            &[
                ("↑↓", "select"),
                ("←→", "column"),
                ("s", "sort"),
                ("e", "explain"),
                ("Enter", "inspect"),
                ("/", "filter"),
                ("t", "tree"),
                ("c", "paths"),
                ("f", "family"),
                ("k", "signal"),
                ("+-", "nice"),
                ("T", "theme"),
                ("D", "graphs"),
                ("?", "help"),
                ("q", "quit"),
            ]
        };
        let mut spans = vec![];
        for (k, what) in keys {
            spans.push(Span::styled(
                format!(" {k} "),
                Style::default().fg(theme::pal().accent).bg(theme::pal().key_bg).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(format!(" {what}   "), theme::dim()));
        }
        if app.paused {
            spans.push(Span::styled("PAUSED ", Style::default().fg(theme::pal().warn).add_modifier(Modifier::BOLD)));
        }
        if !app.filter.is_empty() {
            spans.push(Span::styled(format!("filter: {} ", app.filter), Style::default().fg(theme::pal().warn)));
        }
        Line::from(spans)
    };
    f.render_widget(Paragraph::new(line), area);
}

pub fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2));
    let h = h.min(area.height.saturating_sub(2));
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

fn draw_signals(f: &mut Frame, app: &App, area: Rect) {
    let Some(p) = app.selected_process() else { return };
    let r = centered(area, 92, SIGNALS.len() as u16 + 4);
    f.render_widget(Clear, r);
    let items: Vec<ListItem> = SIGNALS
        .iter()
        .map(|(s, what)| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:<8} {:>2}  ", s.as_str(), *s as i32),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(*what, theme::dim()),
            ]))
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(app.signal_sel));
    let list = List::new(items)
        .block(panel(match app.collector.target() {
            // FR-011: name where the signal goes, not just the PID.
            Some(Target::Pod { namespace, pod, .. }) => format!(
                "send signal to {} ({}) in {namespace}/{pod} container {} — kill -s in the container",
                p.pid,
                p.name,
                crate::app::container(p)
            ),
            Some(Target::Node { node, .. }) => format!("send signal to {} ({}) on node {node}", p.pid, p.name),
            None => format!("send signal to {} ({}) — kill(2)", p.pid, p.name),
        }))
        .highlight_style(theme::selected())
        .highlight_symbol("› ");
    f.render_stateful_widget(list, r, &mut state);
}

fn draw_help(f: &mut Frame, app: &App, area: Rect) {
    let r = centered(area, 78, 37);
    f.render_widget(Clear, r);
    let h = |k: &str, d: &str| {
        Line::from(vec![
            Span::styled(format!("  {k:<14}"), Style::default().fg(theme::pal().accent).add_modifier(Modifier::BOLD)),
            Span::raw(d.to_string()),
        ])
    };
    let s = |t: &str| Line::from(Span::styled(t.to_string(), theme::title()));
    let text = vec![
        s("Process table"),
        h("↑ ↓ PgUp PgDn", "move selection"),
        h("← →", "focus a column (its header turns blue)"),
        h("s  F6  < >", "sort by the focused column (again: reverse)"),
        h("e", "explain the focused column / field: meaning, source, pitfalls"),
        h("[  ]", "scroll the explain panel"),
        h("Enter", "inspect process: Overview Memory Files Threads Env Limits Kernel Raw"),
        h("/", "filter by pid, name, command or user"),
        h("t", "tree view (parent → children)"),
        h("f", "show only the selected process and its descendants"),
        h("c", "show / hide executable paths in COMMAND"),
        h("T", "theme: pick a colour theme and accent, Enter saves"),
        h("D", "graph style: glow, braille, chart, octant"),
        h("k", "send a signal"),
        h("+  -", "renice (lowering nice needs privileges)"),
        h("p", "pause refreshing"),
        h("Esc", "clear filter / scope; leave detail view; back to the pod list (rtop k8s)"),
        h("L", "rtop k8s: next Kubernetes lesson in the explain panel (limits, throttling, OOM…)"),
        h("q  Ctrl-C", "quit"),
        Line::raw(""),
        s("Detail view"),
        h("← → Tab 1-8", "switch tab"),
        h("↑ ↓ j k", "select a field"),
        h("r", "jump to the raw kernel data behind the selected field"),
        Line::raw(""),
        s("Labs (rtop lab <name>)"),
        h("n  b", "next / previous step"),
        h("v", "reveal the answer"),
        Line::raw(""),
        Line::from(Span::styled(
            format!("  Columns: {}", app.columns().iter().map(|c| c.title()).collect::<Vec<_>>().join(" ")),
            theme::dim(),
        )),
        Line::from(Span::styled("  Press any key to close.", theme::dim())),
    ];
    f.render_widget(Paragraph::new(text).block(panel("help")).wrap(Wrap { trim: false }), r);
}

/// The `T` overlay; the k8s picker draws it too.
pub fn draw_theme(f: &mut Frame, chooser: &theme::Chooser, area: Rect) {
    let (g, a) = theme::current();
    let r = centered(area, 96, theme::THEMES.len().max(theme::ACCENTS.len()) as u16 + 6);
    f.render_widget(Clear, r);
    let (gn, an) = theme::names();
    let block = panel(format!("theme · {gn} + {an}"));
    let inner = block.inner(r);
    f.render_widget(block, r);
    let [lists, _, help] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1), Constraint::Length(1)]).areas(inner);
    let [left, right, preview] =
        Layout::horizontal([Constraint::Length(30), Constraint::Length(22), Constraint::Min(30)])
            .spacing(2)
            .areas(lists);
    draw_theme_preview(f, preview);

    let heading = |t: &str, focused: bool| {
        let style = if focused { theme::title().fg(theme::pal().accent) } else { theme::dim() };
        Line::from(Span::styled(t.to_string(), style))
    };
    let row = |focused: bool, current: bool, mut spans: Vec<Span<'static>>, name: &str| {
        let marker = if current { "› " } else { "  " };
        spans.insert(0, Span::styled(marker, Style::default().fg(theme::pal().accent)));
        spans.push(Span::styled(format!(" {name}"), if current { theme::bold() } else { theme::dim() }));
        let line = Line::from(spans);
        if current && focused { line.style(theme::selected()) } else { line }
    };

    let legend = Line::from(Span::styled("  grays  status", theme::faint()));
    let mut themes = vec![heading("theme", chooser.focus == 0), legend];
    themes.extend(theme::THEMES.iter().enumerate().map(|(i, t)| {
        let (grays, status) = t.swatch();
        let block = |c: Color| Span::styled("█", Style::default().fg(c));
        let mut swatch: Vec<Span> = grays.into_iter().map(block).collect();
        swatch.push(Span::raw("  "));
        swatch.extend(status.into_iter().map(block));
        row(chooser.focus == 0, i == g, swatch, t.name)
    }));
    scrolled_list(f, themes, g, left);

    let mut accents = vec![heading("accent", chooser.focus == 1), Line::raw("")];
    accents.extend(theme::ACCENTS.iter().enumerate().map(|(i, accent)| {
        let swatch = vec![Span::styled("████", Style::default().fg(theme::accent_color(i)))];
        row(chooser.focus == 1, i == a, swatch, accent.name)
    }));
    scrolled_list(f, accents, a, right);

    f.render_widget(
        Paragraph::new(Span::styled("↑↓ choose · ←→ theme/accent · Enter save · Esc cancel", theme::dim())),
        help,
    );
}

/// Two heading lines, then a list that keeps item `current` scrolled into view.
fn scrolled_list(f: &mut Frame, mut lines: Vec<Line<'static>>, current: usize, area: Rect) {
    let visible = area.height.saturating_sub(2) as usize;
    let scroll = (current + 1).saturating_sub(visible) as u16;
    let [head, list] = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(area);
    let body = lines.split_off(2);
    f.render_widget(Paragraph::new(lines), head);
    f.render_widget(Paragraph::new(body).scroll((scroll, 0)), list);
}

/// A miniature rtop drawn with the current palette, so every arrow key shows
/// its effect right inside the picker.
fn draw_theme_preview(f: &mut Frame, area: Rect) {
    let p = theme::pal();
    let w = area.width as usize;
    let pad = |s: String| format!("{s:<w$}");
    let lines = vec![
        Line::from(Span::styled("preview", theme::dim())),
        Line::raw(""),
        Line::from(Span::styled("processes", theme::title())),
        Line::from(vec![
            Span::styled(" PID  ", Style::default().fg(p.dim)),
            Span::styled("CPU%▼", Style::default().fg(p.text).bg(p.focus_bg)),
            Span::styled(pad("  COMMAND".into()), Style::default().fg(p.dim)),
        ])
        .style(Style::default().bg(p.header_bg)),
        Line::from(vec![
            Span::styled(" 4242 ", theme::dim()),
            Span::styled(" 52.0", Style::default().fg(theme::level(52.0))),
            Span::styled("  firefox", theme::bold()),
            Span::styled(pad(" --type=gpu".into()), theme::dim()),
        ])
        .style(theme::selected()),
        Line::from(vec![
            Span::styled(" 1001 ", theme::dim()),
            Span::styled("  3.1", theme::text()),
            Span::styled("  zsh", theme::bold()),
            Span::styled(" -l", theme::dim()),
        ]),
        Line::from(vec![
            Span::styled("  733 ", theme::dim()),
            Span::styled("  0.0", theme::faint()),
            Span::styled("  launchd", theme::bold()),
        ]),
        Line::raw(""),
        Line::from([vec![Span::styled("mem ", theme::dim())], header::bar(w.saturating_sub(4).min(24), 72.0)].concat()),
        Line::raw(""),
        Line::from(vec![
            Span::styled(" s ", Style::default().fg(p.accent).bg(p.key_bg).add_modifier(Modifier::BOLD)),
            Span::styled(" sort   ", theme::dim()),
            Span::styled(" T ", Style::default().fg(p.accent).bg(p.key_bg).add_modifier(Modifier::BOLD)),
            Span::styled(" theme", theme::dim()),
        ]),
    ];
    f.render_widget(Paragraph::new(lines), area);
}
