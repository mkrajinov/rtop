//! System header and the process table.

use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Row, Table, TableState};

use super::{panel, theme};
use crate::app::{App, Column, Scope};
use crate::fmt;

/// `/usr/bin/foo --bar` → dim directory, bright executable name, dim args.
/// Paths can contain spaces ("Visual Studio Code.app"), so the executable is
/// located by the process name rather than by the first space.
fn command_spans(cmdline: &str, name: &str, show_path: bool) -> Vec<Span<'static>> {
    let ends_exe = |i: usize| cmdline[i..].starts_with(' ') || i == cmdline.len();
    let exe_end = cmdline
        .match_indices(&format!("/{name}"))
        .map(|(i, m)| i + m.len())
        .find(|&i| ends_exe(i))
        .or_else(|| cmdline.starts_with(name).then_some(name.len()).filter(|&i| ends_exe(i)))
        // comm is truncated (15/16 chars), so long names won't match: for an
        // absolute path, assume the executable runs up to the first " -flag".
        .or_else(|| cmdline.starts_with('/').then(|| cmdline.find(" -").unwrap_or(cmdline.len())))
        .unwrap_or_else(|| cmdline.find(' ').unwrap_or(cmdline.len()));
    let (exe, args) = cmdline.split_at(exe_end);
    let (dir, base) = match exe.rfind('/') {
        Some(i) => exe.split_at(i + 1),
        None => ("", exe),
    };
    let mut v = Vec::new();
    if show_path && !dir.is_empty() {
        v.push(Span::styled(dir.to_string(), theme::faint()));
    }
    v.push(Span::styled(base.to_string(), Style::default().fg(theme::pal().text).add_modifier(Modifier::BOLD)));
    if !args.is_empty() {
        v.push(Span::styled(args.to_string(), Style::default().fg(theme::pal().dim)));
    }
    v
}

/// Widths of the name columns, sized to their longest value on screen.
#[derive(Clone, Copy)]
struct Names {
    pod: u16,
    container: u16,
}

fn width(c: Column, names: Names) -> Constraint {
    match c {
        Column::Pid => Constraint::Length(7),
        Column::Pod => Constraint::Length(names.pod),
        Column::Container => Constraint::Length(names.container),
        Column::User => Constraint::Length(12),
        Column::State => Constraint::Length(2),
        Column::Pri | Column::Nice => Constraint::Length(4),
        Column::Cpu => Constraint::Length(11),
        Column::Mem => Constraint::Length(10),
        Column::Vsz | Column::Rss => Constraint::Length(6),
        // "WRITE/s▼" when sorted.
        Column::Read | Column::Write => Constraint::Length(8),
        Column::Threads => Constraint::Length(4),
        Column::Fds => Constraint::Length(5),
        Column::Time => Constraint::Length(9),
        Column::Command => Constraint::Min(20),
    }
}

/// A 4-cell horizontal bar drawn in eighths of a cell; `frac` 1.0 fills it.
/// Anything above zero shows at least a sliver so small values stay visible.
fn mini_bar(frac: f32) -> String {
    const GLYPHS: [&str; 9] = [" ", "▏", "▎", "▍", "▌", "▋", "▊", "▉", "█"];
    let frac = frac.clamp(0.0, 1.0);
    let mut eighths = (frac * 32.0).round() as usize;
    if eighths == 0 && frac > 0.001 {
        eighths = 1;
    }
    (0..4).map(|k| GLYPHS[eighths.saturating_sub(k * 8).min(8)]).collect()
}

fn is_numeric(c: Column) -> bool {
    !matches!(c, Column::Pod | Column::Container | Column::User | Column::State | Column::Command)
}

fn min_width(c: Column, names: Names) -> u16 {
    match width(c, names) {
        Constraint::Length(n) | Constraint::Min(n) => n + 2,
        _ => 0,
    }
}

/// Drop the least essential columns until the rest fit, so narrow terminals
/// (or an open explain panel) never truncate PIDs.
fn visible_columns(mut cols: Vec<Column>, avail: u16, names: Names) -> Vec<Column> {
    // The I/O columns were asked for with `i`, so they go last.
    const DROP_ORDER: [Column; 8] = [
        Column::Pri,
        Column::Vsz,
        Column::Time,
        Column::Fds,
        Column::Threads,
        Column::User,
        Column::Read,
        Column::Write,
    ];
    for drop in DROP_ORDER {
        if cols.iter().map(|&c| min_width(c, names)).sum::<u16>() <= avail {
            break;
        }
        cols.retain(|&c| c != drop);
    }
    cols
}

pub fn draw_table(f: &mut Frame, app: &mut App, area: Rect) {
    // Names such as `rtop-sb-shard-140-1a2b3` must not be cut.
    let longest = |f: fn(&crate::model::Process) -> &str| {
        app.snapshot.processes.iter().map(|p| f(p).chars().count()).max().unwrap_or(0) as u16
    };
    let names =
        Names { pod: longest(crate::app::pod).clamp(4, 48), container: longest(crate::app::container).clamp(9, 40) };
    let cols = visible_columns(app.columns(), area.width.saturating_sub(2), names);
    let header = Row::new(cols.iter().map(|&c| {
        let arrow = if c == app.sort { if app.sort_desc { "▼" } else { "▲" } } else { "" };
        let mut style = Style::default().fg(theme::pal().dim);
        if c == app.sort {
            style = style.fg(theme::pal().accent);
        }
        if c == app.focus {
            style = style.fg(theme::pal().text).bg(theme::pal().focus_bg);
        }
        let title = format!("{}{arrow}", c.title());
        let line = if is_numeric(c) { Line::from(title).right_aligned() } else { Line::from(title) };
        Cell::from(line).style(style)
    }))
    .style(Style::default().bg(theme::pal().header_bg));
    let me = nix::unistd::getuid().as_raw();

    let rows = app.rows.iter().map(|r| {
        let p = &app.snapshot.processes[r.idx];
        let num = |s: String| Cell::from(Line::from(s).right_aligned());
        // Zeros are noise; dim them so the busy rows stand out.
        let quiet =
            |s: String, zero: bool| if zero { num(s).style(theme::faint()) } else { num(s).style(theme::text()) };
        let unknown = |s: String| {
            if p.restricted {
                Cell::from(Line::from("—").right_aligned()).style(theme::faint())
            } else {
                num(s).style(theme::dim())
            }
        };
        let cpu = if p.restricted {
            unknown(String::new())
        } else {
            let c = p.cpu_percent;
            // One cell is 25% of a core, so a full bar is one busy core.
            let bar = mini_bar(c / 100.0);
            let num_style = match c {
                c if c < 0.05 => theme::faint(),
                c if c < 10.0 => theme::text(),
                c => Style::default().fg(theme::level(c.min(100.0))).add_modifier(Modifier::BOLD),
            };
            Cell::from(
                Line::from(vec![
                    Span::styled(bar, Style::default().fg(theme::level(c.min(100.0)))),
                    Span::styled(format!("{c:>7.1}"), num_style),
                ])
                .right_aligned(),
            )
        };
        let mut cmd = vec![Span::styled(r.prefix.clone(), theme::dim())];
        if p.cmdline.is_empty() {
            cmd.push(Span::styled(format!("<{}> (defunct)", p.name), Style::default().fg(theme::pal().zombie)));
        } else {
            cmd.extend(command_spans(&p.cmdline, &p.name, app.show_path));
        }
        let user_style = match p.uid {
            0 => Style::default().fg(theme::pal().root),
            u if u == me => Style::default().fg(theme::pal().text),
            _ => theme::dim(),
        };
        // Storage bytes per second, coloured like the disk card's read and
        // write graphs. `—`: not readable; blank: no second sample yet.
        let io = |i: usize, color| match (p.io, p.io_rate) {
            (None, _) => Cell::from(Line::from("—").right_aligned()).style(theme::faint()),
            (Some(_), None) => Cell::from(""),
            (Some(_), Some(r)) if r[i] < 1.0 => num("0".into()).style(theme::faint()),
            (Some(_), Some(r)) => num(fmt::bytes(r[i] as u64)).style(Style::default().fg(color)),
        };
        let cells = vec![
            num(p.pid.to_string()).style(theme::dim()),
            Cell::from(crate::app::pod(p).to_string()).style(theme::text()),
            Cell::from(crate::app::container(p).to_string()).style(Style::default().fg(theme::pal().code)),
            Cell::from(p.user.clone()).style(user_style),
            Cell::from(p.state.letter().to_string()).style(theme::state(p.state)),
            unknown(p.priority.to_string()),
            quiet(p.nice.to_string(), p.nice == 0),
            cpu,
            if p.restricted {
                unknown(String::new())
            } else {
                let m = app.mem_percent(p);
                // One cell is 5% of RAM, so a full bar is a fifth of the machine.
                let style = match m {
                    m if m < 0.05 => theme::faint(),
                    m if m < 5.0 => theme::text(),
                    m => Style::default().fg(theme::level(m * 5.0)).add_modifier(Modifier::BOLD),
                };
                Cell::from(
                    Line::from(vec![
                        Span::styled(mini_bar(m / 20.0), Style::default().fg(theme::level(m * 5.0))),
                        Span::styled(format!("{m:>6.1}"), style),
                    ])
                    .right_aligned(),
                )
            },
            unknown(fmt::bytes(p.vsz)),
            if p.restricted {
                unknown(String::new())
            } else {
                num(fmt::bytes(p.rss)).style(Style::default().fg(theme::pal().text))
            },
            io(0, theme::pal().code),
            io(1, theme::pal().zombie),
            unknown(p.threads.to_string()),
            p.fds.map_or(Cell::from(Line::from("—").right_aligned()).style(theme::faint()), |n| {
                num(n.to_string()).style(theme::dim())
            }),
            unknown(fmt::cpu_time(p.cpu_time_ns)),
            Cell::from(Line::from(cmd)),
        ];
        Row::new(Column::ALL.iter().zip(cells).filter(|(c, _)| cols.contains(c)).map(|(_, cell)| cell))
    });

    let mut title = if app.rows.len() == app.snapshot.processes.len() {
        format!("processes {}", app.rows.len())
    } else {
        format!("processes {} of {}", app.rows.len(), app.snapshot.processes.len())
    };
    if app.tree {
        title.push_str(" · tree");
    }
    if let Some(c) = &app.container {
        title.push_str(&format!(" · container {c} (C for the next)"));
    }
    if let Scope::Family(pid) = app.scope {
        title.push_str(&format!(" · family of {pid} (Esc to clear)"));
    }
    if let Some(lab) = &app.lab {
        title.push_str(&format!(" · lab '{}' process group {}", lab.lab.name, lab.pgid));
    }
    let table = Table::new(rows, cols.iter().map(|&c| width(c, names)))
        .header(header)
        .block(panel(&title))
        .column_spacing(2)
        .row_highlight_style(theme::selected());
    let mut state = TableState::default().with_offset(app.table_offset).with_selected(Some(app.selected));
    f.render_stateful_widget(table, area, &mut state);
    app.table_offset = state.offset();
}
