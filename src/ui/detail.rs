//! The per-process inspector: tabs of fields, each traceable to its source.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph, Tabs};

use super::{panel, theme};
use crate::app::{App, DetailRow, Tab};
use crate::fmt;

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(d) = app.detail.as_mut() else { return };
    let (name, notes) = match &d.data {
        Ok(data) => (data.name.clone(), data.notes.clone()),
        Err(e) => (String::new(), vec![e.clone()]),
    };
    let block = panel(format!("PID {} · {name}", d.pid));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(notes.len() as u16),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(inner);

    let tabs = Tabs::new(Tab::ALL.iter().enumerate().map(|(i, t)| format!("{} {}", i + 1, t.title())))
        .select(Tab::ALL.iter().position(|t| *t == d.tab))
        .style(theme::dim())
        .highlight_style(
            Style::default().fg(theme::pal().bright).bg(theme::pal().focus_bg).add_modifier(Modifier::BOLD),
        )
        .divider(" ");
    f.render_widget(tabs, chunks[0]);

    let note_lines: Vec<Line> = notes
        .iter()
        .map(|n| Line::from(Span::styled(format!("⚠ {n}"), Style::default().fg(theme::pal().warn))))
        .collect();
    f.render_widget(Paragraph::new(note_lines), chunks[1]);

    let width = chunks[2].width as usize;
    let n = d.rows().len();
    d.selected = d.selected.min(n.saturating_sub(1));
    let rows = d.rows();
    let hint = d.raw_hint.as_deref();
    let items: Vec<ListItem> = rows.iter().map(|r| item(r, width, hint)).collect();
    let source = match rows.get(d.selected) {
        Some(DetailRow::Field(fl)) => Some(fl.source.clone()),
        _ => None,
    };
    let empty = items.is_empty();
    let list = List::new(items).highlight_style(theme::selected());
    let mut state = ListState::default().with_offset(d.offset).with_selected(Some(d.selected));
    f.render_stateful_widget(list, chunks[2], &mut state);
    d.offset = state.offset();
    if empty {
        let msg = match d.tab {
            Tab::Threads | Tab::Files | Tab::Env => {
                "Nothing to show: the kernel did not let us read this (see the warning above)."
            }
            _ => "Nothing to show.",
        };
        f.render_widget(Paragraph::new(Span::styled(msg, theme::dim())), chunks[2]);
    }

    let src = Line::from(match source {
        Some(s) => {
            vec![Span::styled("source ", theme::title()), Span::styled(s, Style::default().fg(theme::pal().code))]
        }
        None => vec![Span::styled("select a field to see where it comes from", theme::dim())],
    });
    f.render_widget(Paragraph::new(src), chunks[3]);
}

fn item<'a>(row: &DetailRow<'a>, width: usize, hint: Option<&str>) -> ListItem<'a> {
    match row {
        DetailRow::Header(t) => ListItem::new(Line::from(Span::styled(format!("── {t} "), theme::title()))),
        DetailRow::Field(fl) => ListItem::new(Line::from(vec![
            Span::styled(format!("{:<34}", fl.label), theme::dim()),
            Span::raw(fl.value.clone()),
        ])),
        DetailRow::Region(r) => ListItem::new(Line::from(vec![
            Span::styled(format!("{:012x}-{:012x} ", r.start, r.end), theme::dim()),
            Span::raw(format!("{:<8} {:<18} ", r.perms, r.kind)),
            Span::raw(format!("{:>8} ", fmt::bytes(r.end - r.start))),
            Span::styled(
                format!("{:>8} ", r.resident.map(fmt::bytes).unwrap_or_default()),
                Style::default().fg(theme::pal().good),
            ),
            Span::raw(r.path.clone()),
        ])),
        DetailRow::File(o) => ListItem::new(Line::from(vec![
            Span::styled(format!("{:>5}  ", o.fd), Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(format!("{:<8} ", o.kind), Style::default().fg(theme::pal().accent)),
            Span::raw(o.target.clone()),
        ])),
        DetailRow::Thread(t) => ListItem::new(Line::from(vec![
            Span::styled(format!("{:>12}  ", t.tid), theme::dim()),
            Span::raw(format!("{:<20} ", if t.name.is_empty() { "—" } else { &t.name })),
            Span::raw(format!("{:<24} ", t.state)),
            Span::raw(format!("{:>10} ", t.cpu_time_ns.map(fmt::cpu_time).unwrap_or_default())),
            Span::styled(t.priority.map(|p| format!("pri {p}")).unwrap_or_default(), theme::dim()),
        ])),
        DetailRow::Env(k, v) => ListItem::new(Line::from(vec![
            Span::styled(k.to_string(), Style::default().fg(theme::pal().accent)),
            Span::styled("=", theme::dim()),
            Span::raw(v.to_string()),
        ])),
        DetailRow::Raw { text, title: true } => {
            ListItem::new(Line::from(Span::styled(format!("▸ {text}"), theme::title())))
        }
        DetailRow::Raw { text, title: false } => {
            let style = match hint {
                Some(h) if !h.is_empty() && text.contains(h) => {
                    Style::default().bg(theme::pal().focus_bg).add_modifier(Modifier::BOLD)
                }
                _ => Style::default().fg(theme::pal().code),
            };
            ListItem::new(Line::from(Span::styled(format!("  {text}"), style)))
        }
        DetailRow::Text(t) => ListItem::new(
            wrap(t, width.saturating_sub(2))
                .into_iter()
                .map(|l| Line::from(Span::styled(l, Style::default().fg(theme::pal().warn))))
                .collect::<Vec<_>>(),
        ),
    }
}

/// Greedy word wrap for multi-line list items.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(10);
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}
