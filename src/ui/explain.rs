//! The "textbook" side panel.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use super::{panel, theme};
use crate::app::{App, DetailRow, Mode};

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let key = app.explain_key();
    let Some(e) = app.catalog.get(key) else {
        f.render_widget(Paragraph::new(format!("No entry for '{key}' yet.")).block(panel("explain")), area);
        return;
    };
    // A Kubernetes target is a Linux machine too.
    let linux_here = matches!(app.collector.platform(), "Linux" | "Kubernetes");
    let mut lines = vec![Line::from(Span::styled(e.title.clone(), theme::title())), Line::raw("")];

    // What exactly is selected right now, and where its value came from.
    let here: Option<(String, String)> = match (&app.mode, &app.detail) {
        (Mode::Detail, Some(d)) => match d.rows().get(d.selected) {
            Some(DetailRow::Field(fl)) => Some((format!("{} = {}", fl.label, fl.value), fl.source.clone())),
            _ => None,
        },
        _ => app.selected_process().map(|p| {
            let v = match app.focus {
                crate::app::Column::Cpu => format!("{:.1}%", p.cpu_percent),
                crate::app::Column::Mem => format!("{:.1}%", app.mem_percent(p)),
                crate::app::Column::Rss => crate::fmt::bytes_exact(p.rss),
                crate::app::Column::Vsz => crate::fmt::bytes_exact(p.vsz),
                crate::app::Column::State => format!("{} — {}", p.state.letter(), p.state.label()),
                crate::app::Column::Nice => p.nice.to_string(),
                crate::app::Column::Pri => p.priority.to_string(),
                c @ (crate::app::Column::Read | crate::app::Column::Write) => {
                    let i = usize::from(c == crate::app::Column::Write);
                    match (p.io, p.io_rate) {
                        (None, _) => "not read (see below)".into(),
                        (Some(t), None) => {
                            format!("{} so far; rate from the next sample", crate::fmt::bytes_exact(t[i]))
                        }
                        (Some(t), Some(r)) => {
                            format!("{}/s, {} so far", crate::fmt::bytes(r[i] as u64), crate::fmt::bytes_exact(t[i]))
                        }
                    }
                }
                crate::app::Column::Threads => p.threads.to_string(),
                crate::app::Column::Fds => p.fds.map_or("not permitted".into(), |n| n.to_string()),
                crate::app::Column::Time => crate::fmt::cpu_time(p.cpu_time_ns),
                crate::app::Column::User => format!("{} (uid {})", p.user, p.uid),
                crate::app::Column::Pid => p.pid.to_string(),
                crate::app::Column::Container => crate::app::container(p).to_string(),
                crate::app::Column::Pod => crate::app::pod(p).to_string(),
                crate::app::Column::Command => p.cmdline.clone(),
            };
            (format!("PID {} ({}): {v}", p.pid, p.name), String::new())
        }),
    };
    if let Some((value, source)) = here {
        lines.push(Line::from(vec![Span::styled("● ", Style::default().fg(theme::pal().accent)), Span::raw(value)]));
        if !source.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("  from ", theme::dim()),
                Span::styled(source, Style::default().fg(theme::pal().code)),
            ]));
        }
        lines.push(Line::raw(""));
    }

    for para in e.what.split('\n') {
        lines.push(Line::raw(para.to_string()));
    }
    lines.push(Line::raw(""));
    let src = |name: &str, text: &str, here: bool| {
        Line::from(vec![
            Span::styled(
                format!("{name:<6}"),
                Style::default()
                    .fg(if here { theme::pal().accent } else { theme::pal().dim })
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(text.to_string()),
            Span::styled(if here { "  ◆ this machine" } else { "" }, Style::default().fg(theme::pal().accent)),
        ])
    };
    lines.push(src("Linux", &e.linux, linux_here));
    lines.push(src("macOS", &e.macos, !linux_here));

    if !e.pitfalls.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "Watch out",
            Style::default().fg(theme::pal().warn).add_modifier(Modifier::BOLD),
        )));
        for p in &e.pitfalls {
            lines.push(Line::from(vec![
                Span::styled("! ", Style::default().fg(theme::pal().warn)),
                Span::raw(p.clone()),
            ]));
        }
    }
    if !e.try_it.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled("Try it", theme::title())));
        for t in &e.try_it {
            lines.push(Line::from(vec![
                Span::styled("$ ", theme::dim()),
                Span::styled(t.clone(), Style::default().fg(theme::pal().code)),
            ]));
        }
    }
    if !e.man.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![Span::styled("man ", theme::title()), Span::raw(e.man.join("  "))]));
    }

    let p = Paragraph::new(lines)
        .block(panel(format!("explain · {key}  [ ] scroll")))
        .wrap(Wrap { trim: false })
        .scroll((app.explain_scroll, 0));
    f.render_widget(p, area);
}
