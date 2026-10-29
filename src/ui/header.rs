//! The dashboard above the process table: CPU, memory and system cards, and
//! on tall terminals a second row with network, disk and GPU.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{panel, theme};
use crate::app::App;
use crate::fmt;
use crate::model::{Battery, Fan, IoCounters, ProcState, Target};

/// Rows inside each card.
const INNER: usize = 5;

const CARD: u16 = INNER as u16 + 2;

/// Dashboard height for a terminal of `rows`: nothing on short terminals so
/// the process list keeps its rows, one row of cards, or two.
pub fn height(rows: u16) -> u16 {
    match rows {
        0..22 => 0,
        22..38 => CARD,
        _ => CARD * 2,
    }
}

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    // Fixed-width text cards get a fixed width; graphs stretch to fill the
    // rest. Both rows share the column split so the cards line up.
    let columns = || Layout::horizontal([Constraint::Fill(2), Constraint::Fill(1), Constraint::Length(36)]).spacing(1);
    let [top, bottom] = Layout::vertical([Constraint::Length(CARD), Constraint::Length(CARD)]).areas(area);
    let [cpu, mem, sys] = columns().areas(top);
    draw_cpu(f, app, cpu);
    draw_mem(f, app, mem);
    draw_sys(f, app, sys);
    if area.height >= CARD * 2 {
        let sys = &app.snapshot.system;
        let p = theme::pal();
        let [mut net, mut disk, gpu] = columns().areas(bottom);
        // With a battery, network and disk share the space under the cpu
        // card and the battery sits under memory.
        if let Some(b) = &sys.battery {
            [net, disk] = Layout::horizontal([Constraint::Fill(1); 2]).spacing(1).areas(net);
            draw_battery(f, b, columns().areas::<3>(bottom)[1]);
        }
        draw_io(f, net, "network", sys.net.as_ref(), &app.net_hist, [("↓ in", p.code), ("↑ out", p.zombie)], None);
        draw_io(
            f,
            disk,
            "disk",
            sys.disk.as_ref(),
            &app.disk_hist,
            [("read", p.code), ("write", p.zombie)],
            sys.thermal.ssd_c,
        );
        draw_gpu(f, app, gpu);
    }
}

const EIGHTHS: [&str; 9] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];

/// A multi-row bar graph of 0–100% samples, newest on the right. Each row is
/// coloured by the height it represents, so peaks turn amber and red.
pub fn graph(values: &[f32], width: usize, height: usize) -> Vec<Line<'static>> {
    // A one-row sparkline has no height to grade, so colour by value.
    plot(values, width, height, |top, v| theme::level(if height == 1 { v } else { top }))
}

/// A graph of rates with no natural maximum, scaled so the highest visible
/// sample fills the height. Throughput isn't good or bad, so one colour.
fn rate_graph(values: &[f32], width: usize, height: usize, color: Color) -> Vec<Line<'static>> {
    let peak = visible(values, width).iter().fold(0f32, |a, &b| a.max(b));
    let pct: Vec<f32> = values.iter().map(|v| if peak > 0.0 { v / peak * 100.0 } else { 0.0 }).collect();
    plot(&pct, width, height, |_, _| color)
}

/// The newest `width` samples.
fn visible(values: &[f32], width: usize) -> &[f32] {
    &values[values.len().saturating_sub(width)..]
}

/// Draws 0–100 samples; `color(row_top, value)` picks each cell's colour.
fn plot(values: &[f32], width: usize, height: usize, color: impl Fn(f32, f32) -> Color) -> Vec<Line<'static>> {
    let shown = visible(values, width);
    let pad = width - shown.len();
    (0..height)
        .map(|row| {
            let band = 100.0 / height as f32;
            let lo = (height - 1 - row) as f32 * band;
            let mut spans = vec![Span::raw(" ".repeat(pad))];
            spans.extend(shown.iter().map(|&v| {
                let eighths = (((v - lo) / band).clamp(0.0, 1.0) * 8.0).round() as usize;
                if eighths == 0 && row == height - 1 {
                    Span::styled("▁", theme::faint())
                } else {
                    Span::styled(EIGHTHS[eighths], Style::default().fg(color(lo + band, v)))
                }
            }));
            Line::from(spans)
        })
        .collect()
}

fn rate(bytes_per_sec: f32) -> String {
    format!("{}/s", fmt::bytes(bytes_per_sec as u64))
}

/// Two throughput graphs side by side (in | out, read | write), each scaled
/// to its own peak. The title names where the counters come from.
fn draw_io(
    f: &mut Frame,
    area: Rect,
    title: &str,
    counters: Option<&IoCounters>,
    hist: &[std::collections::VecDeque<f32>; 2],
    series: [(&str, Color); 2],
    temp: Option<f32>,
) {
    let source = counters.map_or("no counters on this system", |c| c.source.as_str());
    let mut right = Vec::new();
    if let Some(c) = temp {
        right.extend([Span::raw(" "), temp_span(c), Span::styled(" ·", theme::faint())]);
    }
    right.push(Span::styled(format!(" {source} "), theme::faint()));
    let block = panel(title).title_top(Line::from(right).right_aligned());
    let inner = block.inner(area);
    f.render_widget(block, area);
    if counters.is_none() || inner.height < 2 {
        return;
    }
    let halves: [Rect; 2] = Layout::horizontal([Constraint::Fill(1); 2]).spacing(2).areas(inner);
    for ((half, values), (label, color)) in halves.into_iter().zip(hist).zip(series) {
        let values: Vec<f32> = values.iter().copied().collect();
        let w = half.width as usize;
        let now = values.last().copied().unwrap_or(0.0);
        let peak = visible(&values, w).iter().fold(0f32, |a, &b| a.max(b));
        let cur = rate(now);
        let style = if now < 1.0 { theme::faint() } else { Style::default().fg(color).add_modifier(Modifier::BOLD) };
        let used = label.chars().count() + 1 + cur.len();
        let mut head = vec![Span::styled(format!("{label} "), theme::dim()), Span::styled(cur, style)];
        // The peak is the graph's top; show it when the half has room.
        let top = format!("max {}", rate(peak));
        if used + 2 + top.len() <= w {
            head.push(Span::raw(" ".repeat(w - used - top.len())));
            head.push(Span::styled(top, theme::faint()));
        }
        let mut lines = vec![Line::from(head)];
        lines.extend(rate_graph(&values, w, half.height as usize - 1, color));
        f.render_widget(Paragraph::new(lines), half);
    }
}

fn draw_gpu(f: &mut Frame, app: &App, area: Rect) {
    let gpu = app.snapshot.system.gpu.as_ref();
    let mut block = panel("gpu");
    if let Some(g) = gpu {
        let mut title = vec![Span::styled(
            format!(" {:.0}% ", g.busy),
            Style::default().fg(theme::level(g.busy)).add_modifier(Modifier::BOLD),
        )];
        if let Some(m) = g.mem_used {
            title.push(Span::styled(format!("{} ", fmt::bytes(m)), theme::dim()));
        }
        if let Some(c) = app.snapshot.system.thermal.gpu_c {
            title.extend([temp_span(c), Span::raw(" ")]);
        }
        block = block.title_top(Line::from(title).right_aligned());
    }
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(g) = gpu else {
        let text = match app.collector.platform() {
            "Linux" => "No GPU counters. amdgpu exposes gpu_busy_percent in sysfs; NVIDIA only through NVML.",
            "macOS" => "No IOAccelerator reports utilisation.",
            _ => "No GPU counters.",
        };
        f.render_widget(Paragraph::new(text).style(theme::dim()).wrap(ratatui::widgets::Wrap { trim: true }), inner);
        return;
    };
    let hist: Vec<f32> = app.gpu_hist.iter().copied().collect();
    let mut lines = vec![Line::from(Span::styled(g.name.clone(), theme::dim()))];
    lines.extend(graph(&hist, inner.width as usize, inner.height.saturating_sub(1) as usize));
    f.render_widget(Paragraph::new(lines), inner);
}

/// A horizontal gauge of small squares, gradient-coloured along its length.
pub fn bar(width: usize, pct: f32) -> Vec<Span<'static>> {
    let fill = ((pct.clamp(0.0, 100.0) / 100.0) * width as f32).round() as usize;
    let mut v: Vec<Span> = (0..fill)
        .map(|i| Span::styled("■", Style::default().fg(theme::level((i as f32 + 0.5) / width as f32 * 100.0))))
        .collect();
    v.push(Span::styled("■".repeat(width - fill), theme::faint()));
    v
}

/// Temperatures turn amber from 67 °C and red from 90 °C.
fn temp_span(c: f32) -> Span<'static> {
    Span::styled(format!("{c:.0}°C"), Style::default().fg(theme::level((c - 30.0) * 4.0 / 3.0)))
}

fn pct_span(pct: f32, width: usize) -> Span<'static> {
    let style = if pct < 1.0 { theme::faint() } else { Style::default().fg(theme::level(pct)) };
    Span::styled(format!("{:>width$}", format!("{pct:.0}%")), style)
}

fn draw_cpu(f: &mut Frame, app: &App, area: Rect) {
    let info = &app.snapshot.system.cpu;
    let kinds = &info.kinds;
    let total = app.cpu_hist.back().copied().unwrap_or(0.0);
    let pct_style = |p: f32| Style::default().fg(theme::level(p)).add_modifier(Modifier::BOLD);
    // Average per core kind: on Apple Silicon the E-cores are often pinned at
    // 100% by background work while the P-cores idle.
    let mut right = Vec::new();
    for kind in ['P', 'E'] {
        let usage: Vec<f32> = app.core_usage.iter().zip(kinds).filter(|&(_, &k)| k == kind).map(|(&u, _)| u).collect();
        if !usage.is_empty() {
            let avg = usage.iter().sum::<f32>() / usage.len() as f32;
            right.push(Span::styled(format!(" {}{kind} ", usage.len()), theme::dim()));
            right.push(Span::styled(format!("{avg:.0}%"), pct_style(avg)));
            right.push(Span::styled(" ·", theme::faint()));
        }
    }
    if right.is_empty() {
        right.push(Span::styled(format!(" {} cores ·", app.core_usage.len()), theme::dim()));
    }
    if let Some(mhz) = info.mhz {
        right.push(Span::styled(format!(" {:.2} GHz ·", mhz / 1000.0), theme::dim()));
    }
    if let Some(c) = app.snapshot.system.thermal.cpu_c {
        right.extend([Span::raw(" "), temp_span(c), Span::styled(" ·", theme::faint())]);
    }
    right.push(Span::styled(format!(" {total:.0}% "), pct_style(total)));
    let mut block = panel("cpu").title_top(Line::from(right).right_aligned());
    if !info.model.is_empty() {
        block = block.title_top(Line::from(Span::styled(format!(" {} ", info.model), theme::dim())));
    }
    let inner = block.inner(area);
    f.render_widget(block, area);

    let n = app.core_usage.len();
    let cols = n.div_ceil(INNER).max(1);
    // "P10 " needs a column more than " 9 ".
    let label_w = if kinds.is_empty() { 3 } else { 4 };
    let cell = label_w + 16;
    let cores_w = (cols * cell) as u16;
    let [g, _, c] =
        Layout::horizontal([Constraint::Min(10), Constraint::Length(2), Constraint::Length(cores_w)]).areas(inner);

    let hist: Vec<f32> = app.cpu_hist.iter().copied().collect();
    f.render_widget(Paragraph::new(graph(&hist, g.width as usize, g.height as usize)), g);

    let rows: Vec<Line> = (0..INNER)
        .map(|r| {
            let mut spans = Vec::new();
            for col in 0..cols {
                let i = col * INNER + r;
                if let Some(&pct) = app.core_usage.get(i) {
                    match kinds.get(i) {
                        Some(&k) => {
                            let style = if k == 'P' { theme::text() } else { theme::dim() };
                            spans.push(Span::styled(k.to_string(), style));
                            spans.push(Span::styled(format!("{i:<2} "), theme::dim()));
                        }
                        None => spans.push(Span::styled(format!("{i:>2} "), theme::dim())),
                    }
                    spans.extend(bar(10, pct));
                    spans.push(pct_span(pct, 5));
                    spans.push(Span::raw(" "));
                }
            }
            Line::from(spans)
        })
        .collect();
    f.render_widget(Paragraph::new(rows), c);
}

/// Charge, health and power. Health is the full-charge capacity as a share
/// of what the battery held new; the gauge learns it over charge cycles.
fn draw_battery(f: &mut Frame, b: &Battery, area: Rect) {
    let block =
        panel("battery").title_top(Line::from(Span::styled(format!(" {} ", b.source), theme::faint())).right_aligned());
    let inner = block.inner(area);
    f.render_widget(block, area);
    let w = inner.width as usize;
    // Low charge is the bad end here, so invert the usual scale.
    let charge = theme::level(100.0 - b.percent);
    let label = |t: &str| Span::styled(format!("{t:<8}"), theme::dim());
    let sep = || Span::styled(" · ", theme::faint());

    let mut state = b.state.clone();
    if let Some(m) = b.minutes {
        let what = if b.state == "charging" { "to full" } else { "left" };
        state = format!("{state} · {}:{:02} {what}", m / 60, m % 60);
    }
    let pct = format!("{:.0}%", b.percent);
    let gap = w.saturating_sub(8 + state.chars().count() + pct.len() + 1);
    let mut lines = vec![
        Line::from(vec![
            label("charge"),
            Span::styled(state, theme::text()),
            Span::raw(" ".repeat(gap + 1)),
            Span::styled(pct, Style::default().fg(charge).add_modifier(Modifier::BOLD)),
        ]),
        {
            let fill = (b.percent.clamp(0.0, 100.0) / 100.0 * w as f32).round() as usize;
            Line::from(vec![
                Span::styled("■".repeat(fill), Style::default().fg(charge)),
                Span::styled("■".repeat(w - fill), theme::faint()),
            ])
        },
    ];

    let mut health = vec![label("health")];
    if let Some((full, design, unit)) = b.capacity {
        let h = full as f32 / design as f32 * 100.0;
        health.push(Span::styled(format!("{h:.0}%"), Style::default().fg(theme::level(100.0 - h))));
        health.push(Span::styled(format!("  {full} / {design} {unit}"), theme::dim()));
    }
    if let Some(c) = b.cycles {
        health.push(sep());
        health.push(Span::styled(format!("{c} cycles"), theme::dim()));
    }
    lines.push(Line::from(health));

    let mut power = vec![label("power")];
    if let Some(wt) = b.system_watts {
        power.push(Span::styled(format!("{wt:.1} W"), theme::bold()));
        power.push(Span::styled(" system", theme::dim()));
    }
    if let Some(a) = b.adapter_watts {
        if power.len() > 1 {
            power.push(sep());
        }
        power.push(Span::styled(format!("{a} W adapter"), theme::dim()));
    }
    lines.push(Line::from(power));

    let mut cell = vec![label("cell")];
    let parts: Vec<String> = [
        b.voltage_mv.map(|v| format!("{:.2} V", v as f32 / 1000.0)),
        b.current_ma.map(|a| format!("{a:+} mA")),
        b.temp_c.map(|t| format!("{t:.1} °C")),
    ]
    .into_iter()
    .flatten()
    .collect();
    cell.push(Span::styled(parts.join("  "), theme::dim()));
    lines.push(Line::from(cell));
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_mem(f: &mut Frame, app: &App, area: Rect) {
    let sys = &app.snapshot.system;
    let pct = |a: u64, b: u64| if b == 0 { 0.0 } else { a as f32 / b as f32 * 100.0 };
    let (mp, sp) = (pct(sys.mem_used, sys.mem_total), pct(sys.swap_used, sys.swap_total));
    let block = panel("memory")
        .title_top(Line::from(Span::styled(format!(" {} ", fmt::bytes(sys.mem_total)), theme::dim())).right_aligned());
    let inner = block.inner(area);
    f.render_widget(block, area);
    let w = inner.width as usize;

    let row = |label: &str, used: u64, total: u64, p: f32| {
        let nums = format!("{} / {}", fmt::bytes(used), fmt::bytes(total));
        let gap = w.saturating_sub(label.len() + nums.len() + 5);
        Line::from(vec![
            Span::styled(label.to_string(), theme::dim()),
            Span::raw(" ".repeat(gap.max(1))),
            Span::styled(nums, theme::text()),
            pct_span(p, 5),
        ])
    };
    let hist: Vec<f32> = app.mem_hist.iter().copied().collect();
    let mut lines = vec![
        row("used", sys.mem_used, sys.mem_total, mp),
        Line::from(bar(w, mp)),
        row("swap", sys.swap_used, sys.swap_total, sp),
        Line::from(bar(w, sp)),
    ];
    lines.extend(graph(&hist, w, 1));
    f.render_widget(Paragraph::new(lines), inner);
}

/// Fan speeds, coloured by how close each is to its maximum.
fn fans(fans: &[Fan]) -> Line<'static> {
    let mut spans = vec![Span::styled(format!("{:<7}", "fans"), theme::dim())];
    if fans.is_empty() {
        spans.push(Span::styled("none reported", theme::faint()));
    }
    for f in fans {
        let pct = if f.max > 0.0 { f.rpm / f.max * 100.0 } else { 0.0 };
        let style = if f.rpm < 1.0 { theme::faint() } else { Style::default().fg(theme::level(pct)) };
        spans.push(Span::styled(format!("{:.0} ", f.rpm), style));
    }
    if !fans.is_empty() {
        spans.push(Span::styled("rpm", theme::dim()));
    }
    Line::from(spans)
}

fn draw_sys(f: &mut Frame, app: &App, area: Rect) {
    let sys = &app.snapshot.system;
    let block = panel("system").title_top(
        Line::from(Span::styled(
            format!(" up {} · {:.1}s ", fmt::uptime(sys.uptime_secs), app.tick.as_secs_f32()),
            theme::dim(),
        ))
        .right_aligned(),
    );
    let inner = block.inner(area);
    f.render_widget(block, area);

    let cores = sys.cores.len().max(1) as f64;
    // Load is a run-queue length: compare it with the number of cores.
    let load = |l: f64| {
        Span::styled(
            format!("{l:<7.2}"),
            Style::default().fg(theme::level((l / cores * 100.0) as f32)).add_modifier(Modifier::BOLD),
        )
    };
    let procs = &app.snapshot.processes;
    let count = |s: ProcState| procs.iter().filter(|p| p.state == s).count();
    let (running, zombies, dstate) = (count(ProcState::Running), count(ProcState::Zombie), count(ProcState::DiskSleep));
    let label = |t: &str| Span::styled(format!("{t:<7}"), theme::dim());
    let dot = |n: usize, what: &str, c| {
        let style = if n == 0 { theme::faint() } else { Style::default().fg(c) };
        Span::styled(format!("● {n} {what}  "), style)
    };

    let lines = vec![
        Line::from(vec![label("load"), load(sys.load[0]), load(sys.load[1]), load(sys.load[2])]),
        Line::from(vec![label(""), Span::styled(format!("{:<7}{:<7}{:<7}", "1m", "5m", "15m"), theme::faint())]),
        Line::from(vec![
            label("tasks"),
            Span::styled(format!("{}  ", procs.len()), theme::bold()),
            dot(running, "running", theme::pal().good),
        ]),
        Line::from(vec![label(""), dot(zombies, "zombie", theme::pal().zombie), dot(dstate, "in D", theme::pal().bad)]),
        fans(&sys.thermal.fans),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

/// The line above the cards in Kubernetes mode: which cluster, pod and node
/// this is (FR-008), whether the data is stale and what rtop changed or
/// could not read (FR-012).
pub fn draw_target(f: &mut Frame, app: &App, area: Rect) {
    let Some(target) = app.collector.target() else { return };
    let link = app.collector.link().unwrap_or_default();
    let p = theme::pal();
    // A context taken from the kubeconfig may not be the cluster you meant.
    let ctx_style = if link.implicit_context {
        Style::default().fg(p.warn).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.accent).add_modifier(Modifier::BOLD)
    };
    let sep = || Span::styled(" · ", theme::faint());
    let mut spans = vec![Span::styled("⎈ ", Style::default().fg(p.accent))];
    match target {
        Target::Pod { context, namespace, pod, node, .. } => {
            spans.push(Span::styled(context.clone(), ctx_style));
            if link.implicit_context {
                spans.push(Span::styled(" (current context)", Style::default().fg(p.warn)));
            }
            spans.extend([sep(), Span::styled(format!("{namespace}/{pod}"), theme::bold())]);
            if let Some(n) = node {
                spans.extend([sep(), Span::styled(format!("node {n}"), theme::dim())]);
            }
        }
        Target::Node { context, node } => {
            spans.push(Span::styled(context.clone(), ctx_style));
            spans.extend([sep(), Span::styled(format!("node {node}"), theme::bold())]);
        }
    }
    if let Some(age) = link.stale {
        spans.extend([sep(), Span::styled(format!("stale {}s", age.as_secs()), Style::default().fg(p.bad))]);
    }
    for n in &link.notes {
        spans.extend([sep(), Span::styled(n.clone(), Style::default().fg(p.warn))]);
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}
