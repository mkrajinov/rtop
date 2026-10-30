//! The `rtop k8s` picker: two views under a tab line. Pods shows
//! namespaces on the left and their pods on the right; Nodes shows the
//! cluster's nodes across the full width.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, List, ListItem, ListState, Padding, Paragraph, Row, Table, TableState,
};

use super::theme;
use crate::kube::msg::{NodeSummary, PodSummary};
use crate::picker::{Pane, Picker, View};

pub fn draw(f: &mut Frame, p: &Picker) {
    let full = f.area();
    let area = Rect { x: full.x + 1, width: full.width.saturating_sub(2), ..full };
    let rows =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(3), Constraint::Length(1)])
            .split(area);
    draw_context(f, p, rows[0]);
    draw_tabs(f, p, rows[1]);
    if p.view == View::Nodes {
        draw_nodes(f, p, rows[2]);
    } else {
        let ns_width = p.namespaces.iter().map(|n| n.len()).max().unwrap_or(0).clamp(16, 32) as u16 + 6;
        let cols = Layout::horizontal([Constraint::Length(ns_width), Constraint::Min(20)]).split(rows[2]);
        draw_namespaces(f, p, cols[0]);
        draw_pods(f, p, cols[1]);
    }
    draw_footer(f, p, rows[3]);
    theme::finalize(f.buffer_mut());
}

fn draw_context(f: &mut Frame, p: &Picker, area: Rect) {
    let pal = theme::pal();
    // The same warning as the pod view: the kubeconfig's current context
    // may not be the cluster you meant.
    let ctx = if p.implicit_context { pal.warn } else { pal.accent };
    let mut spans = vec![
        Span::styled("⎈ ", Style::default().fg(pal.accent)),
        Span::styled(p.context.clone(), Style::default().fg(ctx).add_modifier(Modifier::BOLD)),
    ];
    if p.implicit_context {
        spans.push(Span::styled(" (current context)", Style::default().fg(pal.warn)));
    }
    spans.extend([Span::styled(" · ", theme::faint()), Span::styled(p.server.clone(), theme::dim())]);
    if let Some(n) = &p.ns_note {
        spans.extend([Span::styled(" · ", theme::faint()), Span::styled(n.clone(), Style::default().fg(pal.warn))]);
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// `1 Pods  2 Nodes`: the shown view is a filled chip, the other plain.
fn draw_tabs(f: &mut Frame, p: &Picker, area: Rect) {
    let pal = theme::pal();
    let mut spans = vec![];
    for (key, view, name) in [("1", View::Pods, "Pods"), ("2", View::Nodes, "Nodes")] {
        let style = if p.view == view {
            Style::default().fg(pal.accent).bg(pal.key_bg).add_modifier(Modifier::BOLD)
        } else {
            theme::dim()
        };
        spans.push(Span::styled(format!(" {key} "), theme::faint()));
        spans.push(Span::styled(format!(" {name} "), style));
        spans.push(Span::raw("  "));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The focused pane gets the strong highlight; the other one keeps its
/// choice visible in the accent colour only.
fn highlight(focused: bool) -> Style {
    if focused { theme::selected() } else { Style::default().fg(theme::pal().accent).add_modifier(Modifier::BOLD) }
}

fn pane(name: &str, focused: bool) -> Block<'static> {
    let style = if focused { theme::title() } else { theme::dim() };
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::faint())
        .padding(Padding::horizontal(1))
        .title(Span::styled(format!(" {name} "), style))
}

fn draw_namespaces(f: &mut Frame, p: &Picker, area: Rect) {
    let focused = p.focus == Pane::Namespaces;
    let items: Vec<ListItem> = p
        .visible_namespaces()
        .iter()
        .map(|n| match n.as_str() {
            "" => ListItem::new(Span::styled("all namespaces", theme::dim())),
            n => ListItem::new(n.to_string()),
        })
        .collect();
    let list = List::new(items).block(pane("Namespaces", focused)).highlight_style(highlight(focused));
    let mut state = ListState::default().with_selected(Some(p.ns_sel));
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_pods(f: &mut Frame, p: &Picker, area: Rect) {
    let focused = p.focus == Pane::Pods;
    let all = p.ns.is_empty();
    let rows = p.visible_pods();
    let title = match (&p.pods, all) {
        (Some(Ok(_)), true) => format!("Pods in all namespaces ({})", rows.len()),
        (Some(Ok(_)), false) => format!("Pods in {} ({})", p.ns, rows.len()),
        (_, true) => "Pods in all namespaces".into(),
        _ => format!("Pods in {}", p.ns),
    };
    let block = pane(&title, focused);
    let message = match &p.pods {
        None => Some(("loading…".to_string(), theme::dim())),
        Some(Err(e)) => Some((e.clone(), Style::default().fg(theme::pal().bad))),
        Some(Ok(_)) if rows.is_empty() && p.pod_filter.is_empty() => Some(("no pods".into(), theme::dim())),
        Some(Ok(_)) if rows.is_empty() => Some((format!("no pod matches “{}”", p.pod_filter), theme::dim())),
        _ => None,
    };
    if let Some((text, style)) = message {
        f.render_widget(Paragraph::new(Span::styled(text, style)).block(block), area);
        return;
    }
    let mut header = vec!["NAME", "READY", "STATUS", "RESTARTS", "AGE", "NODE"];
    // Names matter most: as wide as the longest one, the node gets the rest.
    let longest = |f: fn(&PodSummary) -> usize| rows.iter().map(|p| f(p)).max().unwrap_or(0) as u16;
    let mut widths = vec![
        Constraint::Length(longest(|p| p.name.len()).clamp(4, 40)),
        Constraint::Length(5),
        // ContainerCreating, CrashLoopBackOff
        Constraint::Length(17),
        Constraint::Length(8),
        Constraint::Length(4),
        Constraint::Fill(1),
    ];
    if all {
        header.insert(0, "NAMESPACE");
        widths.insert(0, Constraint::Length(longest(|p| p.ns.len()).clamp(9, 24)));
    }
    let body: Vec<Row> = rows.iter().map(|pod| row(p, pod, all)).collect();
    let table = Table::new(body, widths)
        .header(Row::new(header).style(theme::dim().add_modifier(Modifier::BOLD)))
        .column_spacing(2)
        .block(block)
        .row_highlight_style(highlight(focused));
    let mut state = TableState::default().with_selected(Some(p.pod_sel));
    f.render_stateful_widget(table, area, &mut state);
}

fn draw_nodes(f: &mut Frame, p: &Picker, area: Rect) {
    let rows = p.visible_nodes();
    let title = match &p.nodes {
        Some(Ok(_)) => format!("Nodes ({})", rows.len()),
        _ => "Nodes".into(),
    };
    let block = pane(&title, true);
    let message = match &p.nodes {
        None => Some(("loading…".to_string(), theme::dim())),
        Some(Err(e)) => Some((e.clone(), Style::default().fg(theme::pal().bad))),
        Some(Ok(_)) if rows.is_empty() && p.node_filter.is_empty() => Some(("no nodes".into(), theme::dim())),
        Some(Ok(_)) if rows.is_empty() => Some((format!("no node matches “{}”", p.node_filter), theme::dim())),
        _ => None,
    };
    if let Some((text, style)) = message {
        f.render_widget(Paragraph::new(Span::styled(text, style)).block(block), area);
        return;
    }
    let longest = rows.iter().map(|n| n.name.len()).max().unwrap_or(0) as u16;
    let widths = [
        Constraint::Length(longest.clamp(4, 48)),
        // Ready,SchedulingDisabled
        Constraint::Length(24),
        Constraint::Length(13),
        Constraint::Length(4),
        Constraint::Length(8),
        Constraint::Length(4),
        Constraint::Fill(1),
    ];
    let header = ["NAME", "STATUS", "ROLES", "AGE", "VERSION", "CPU", "MEMORY"];
    let body: Vec<Row> = rows.iter().map(|n| node_row(p, n)).collect();
    let table = Table::new(body, widths)
        .header(Row::new(header).style(theme::dim().add_modifier(Modifier::BOLD)))
        .column_spacing(2)
        .block(block)
        .row_highlight_style(highlight(true));
    let mut state = TableState::default().with_selected(Some(p.node_sel));
    f.render_stateful_widget(table, area, &mut state);
}

fn node_row<'a>(p: &Picker, n: &'a NodeSummary) -> Row<'a> {
    let pal = theme::pal();
    let mut status = if n.ready { "Ready".to_string() } else { "NotReady".to_string() };
    if n.unschedulable {
        status.push_str(",SchedulingDisabled");
    }
    let color = match (n.ready, n.unschedulable) {
        (true, false) => pal.good,
        (true, true) => pal.warn,
        _ => pal.bad,
    };
    let roles = if n.roles.is_empty() { "<none>".to_string() } else { n.roles.join(",") };
    let memory = n.memory.as_deref().and_then(crate::fmt::quantity_bytes).map(crate::fmt::bytes).unwrap_or_default();
    Row::new(vec![
        Cell::from(Span::styled(n.name.as_str(), if n.ready { theme::bold() } else { theme::dim() })),
        Cell::from(Span::styled(status, Style::default().fg(color))),
        Cell::from(Span::styled(roles, theme::dim())),
        Cell::from(n.created.map_or_else(String::new, |t| short_age(p.now.saturating_sub(t.max(0) as u64)))),
        Cell::from(Span::styled(n.version.as_str(), theme::dim())),
        Cell::from(n.cpu.clone().unwrap_or_default()),
        Cell::from(memory),
    ])
}

fn row<'a>(p: &Picker, pod: &'a PodSummary, all: bool) -> Row<'a> {
    let pal = theme::pal();
    let running = pod.phase == "Running";
    let status = match &pod.reason {
        // kubectl shows the stuck container's reason instead of the phase.
        Some(r) if !running || pod.ready < pod.containers => r.clone(),
        _ => pod.phase.clone(),
    };
    let status_color = match (running, pod.ready == pod.containers) {
        (true, true) => pal.good,
        (true, false) => pal.warn,
        _ if pod.phase == "Succeeded" => pal.dim,
        _ if pod.phase == "Pending" && pod.reason.is_none() => pal.warn,
        _ => pal.bad,
    };
    let name = if running { theme::bold() } else { theme::dim() };
    let mut cells = vec![
        Cell::from(Span::styled(pod.name.as_str(), name)),
        Cell::from(format!("{}/{}", pod.ready, pod.containers)),
        Cell::from(Span::styled(status, Style::default().fg(status_color))),
        Cell::from(pod.restarts.to_string()),
        Cell::from(pod.created.map_or_else(String::new, |t| short_age(p.now.saturating_sub(t.max(0) as u64)))),
        Cell::from(Span::styled(pod.node.clone().unwrap_or_default(), theme::dim())),
    ];
    if all {
        cells.insert(0, Cell::from(Span::styled(pod.ns.as_str(), theme::dim())));
    }
    Row::new(cells)
}

/// kubectl's AGE column: `45s`, `12m`, `3h`, `5d`.
fn short_age(s: u64) -> String {
    match s {
        0..120 => format!("{s}s"),
        120..7200 => format!("{}m", s / 60),
        7200..172_800 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

fn draw_footer(f: &mut Frame, p: &Picker, area: Rect) {
    let pal = theme::pal();
    let line = if let Some(node) = &p.confirm {
        // The one place the picker writes to the cluster: say exactly what.
        Line::from(vec![
            Span::styled(" create a hostPID helper pod ", Style::default().fg(pal.warn)),
            Span::styled(
                format!("in {} / {}", p.context, p.helper_ns),
                Style::default().fg(pal.warn).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" on node {node}?"), Style::default().fg(pal.warn)),
            Span::styled("   y create · any other key cancels", theme::dim()),
        ])
    } else if let Some(target) = &p.connecting {
        Line::from(vec![
            Span::styled(format!(" connecting to {target} …"), Style::default().fg(pal.accent)),
            Span::styled("   Esc cancel", theme::dim()),
        ])
    } else if let Some(msg) = p.status_text() {
        Line::from(Span::styled(format!(" {msg}"), Style::default().fg(pal.warn)))
    } else if p.filtering {
        let (what, text) = p.filter();
        Line::from(vec![
            Span::styled(format!(" {what} filter: "), theme::title()),
            Span::raw(text.to_string()),
            Span::styled("▏", Style::default().fg(pal.accent)),
            Span::styled("   Enter keep · Esc clear", theme::dim()),
        ])
    } else {
        let keys: &[_] = match p.view {
            View::Pods => &[
                ("↑↓", "select"),
                ("Tab", "pane"),
                ("/", "filter"),
                ("Enter", "open"),
                ("2", "nodes"),
                ("r", "refresh"),
                ("q", "quit"),
            ],
            View::Nodes => {
                &[("↑↓", "select"), ("/", "filter"), ("Enter", "open"), ("1", "pods"), ("r", "refresh"), ("q", "quit")]
            }
        };
        let mut spans = vec![];
        for (k, what) in keys {
            spans.push(Span::styled(
                format!(" {k} "),
                Style::default().fg(pal.accent).bg(pal.key_bg).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(format!(" {what}   "), theme::dim()));
        }
        Line::from(spans)
    };
    f.render_widget(Paragraph::new(line), area);
}

#[cfg(test)]
mod tests {
    #[test]
    fn ages_read_like_kubectl() {
        let got: Vec<_> = [5, 119, 600, 7199, 7200, 200_000].map(super::short_age).into();
        assert_eq!(got, ["5s", "119s", "10m", "119m", "2h", "2d"]);
    }
}
