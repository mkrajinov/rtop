//! The dashboard above the process table: CPU, memory and system cards, and
//! on tall terminals a second row with network, disk and GPU.

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Chart, Dataset, GraphType, Paragraph};
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{panel, theme};
use crate::app::App;
use crate::fmt;
use crate::model::{Battery, ContainerUsage, Fan, IoCounters, ProcState, Target};

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
    // One row of cards gets the whole area; splitting a single row's height
    // between two rows squeezed the top cards to half their size.
    let [top, bottom] = Layout::vertical([Constraint::Length(CARD), Constraint::Fill(1)]).areas(area);
    if !app.snapshot.containers.is_empty() {
        return draw_pod(f, app, top, bottom, area.height >= CARD * 2);
    }
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

/// Pod mode: what the containers may use comes first. /proc/stat and
/// /proc/meminfo describe the whole node, so the cpu and memory cards say
/// "node" and move aside.
fn draw_pod(f: &mut Frame, app: &App, top: Rect, bottom: Rect, two_rows: bool) {
    let columns = || Layout::horizontal([Constraint::Fill(2), Constraint::Fill(1), Constraint::Length(36)]).spacing(1);
    let [containers, mem, sys] = columns().areas(top);
    draw_containers(f, &app.snapshot.containers, containers);
    draw_mem(f, app, mem);
    draw_sys(f, app, sys);
    if two_rows {
        let p = theme::pal();
        let [net, cpu] = Layout::horizontal([Constraint::Fill(2), Constraint::Fill(1)]).spacing(1).areas(bottom);
        let s = &app.snapshot.system;
        draw_io(f, net, "network", s.net.as_ref(), &app.net_hist, [("↓ in", p.code), ("↑ out", p.zombie)], None);
        draw_cpu(f, app, cpu);
    }
}

/// `0.49 / 0.5` cores, or `0.49 · no limit`.
fn cores(used: Option<f64>, limit: Option<f64>) -> String {
    let used = used.map_or("…".into(), |c| format!("{c:.2}"));
    match limit {
        Some(l) => format!("{used} / {}", trim_num(l)),
        None => format!("{used} · no limit"),
    }
}

/// `↻ 2 last OOMKilled`: restarts and why the container last ended.
fn restarts(spans: &mut Vec<Span<'static>>, c: &ContainerUsage) {
    let p = theme::pal();
    spans.push(Span::styled(
        format!("↻ {}", c.restarts),
        if c.restarts > 0 { Style::default().fg(p.warn) } else { theme::faint() },
    ));
    // OOMKilled is the one to notice.
    if let Some(why) = c.last_termination.as_deref().and_then(termination_reason) {
        let style = if why == "OOMKilled" { Style::default().fg(p.bad) } else { theme::dim() };
        spans.push(Span::styled(format!(" last {why}"), style));
    }
}

/// `terminated: OOMKilled (exit 137)` → `OOMKilled`.
fn termination_reason(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("terminated: ")?;
    Some(rest.split(" (").next().unwrap_or(rest))
}

/// A usage bar against a limit; without a limit there is nothing to fill.
fn bar_or_gap(spans: &mut Vec<Span<'static>>, width: usize, pct: Option<f32>) {
    if width == 0 {
        return;
    }
    match pct {
        Some(p) => spans.extend(bar(width, p)),
        None => spans.push(Span::raw(" ".repeat(width))),
    }
    spans.push(Span::raw(" "));
}

fn trim_num(v: f64) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// One row per container: CPU and memory against the cgroup's limits,
/// throttling, OOM kills and restarts (User Story 4).
fn draw_containers(f: &mut Frame, list: &[ContainerUsage], area: Rect) {
    let p = theme::pal();
    let block = panel("containers")
        .title_top(Line::from(Span::styled(" cgroup: cpu.stat cpu.max memory.* ", theme::dim())).right_aligned());
    let inner = block.inner(area);
    f.render_widget(block, area);
    let w = inner.width as usize;
    let name_w = list.iter().map(|c| c.name.chars().count()).max().unwrap_or(0).clamp(3, 16);
    // Bars only when there is room for them.
    let bar_w = if w >= name_w + 98 { 6 } else { 0 };
    let shown = INNER.min(list.len());
    let mut lines: Vec<Line> = Vec::new();
    for (i, c) in list.iter().enumerate().take(shown) {
        if i == INNER - 1 && list.len() > INNER {
            lines.push(Line::from(Span::styled(format!("+{} more containers", list.len() - i), theme::dim())));
            break;
        }
        let name: String = c.name.chars().take(name_w).collect();
        let mut spans = vec![Span::styled(format!("{name:<name_w$}  "), Style::default().fg(p.code))];
        if c.unreadable {
            // Only the pod spec is known: show its limits and why.
            let cpu = c.cpu_limit.map_or("none".into(), trim_num);
            let mem = c.mem_max.map_or("none".into(), fmt::bytes);
            spans.push(Span::styled(format!("limits cpu {cpu} mem {mem}  "), theme::dim()));
            restarts(&mut spans, c);
            spans.push(Span::styled(format!("  {}", c.source), theme::faint()));
            lines.push(Line::from(spans));
            continue;
        }
        let cpu_pct = match (c.cpu_cores, c.cpu_limit) {
            (Some(u), Some(l)) if l > 0.0 => Some((u / l * 100.0) as f32),
            _ => None,
        };
        spans.push(Span::styled("cpu ", theme::dim()));
        bar_or_gap(&mut spans, bar_w, cpu_pct);
        spans.push(Span::styled(format!("{:<15}", cores(c.cpu_cores, c.cpu_limit)), theme::text()));
        // Throttling only means something under a quota.
        match (c.cpu_limit, c.throttled_pct) {
            (Some(_), Some(t)) => spans.push(Span::styled(
                format!("thr {:>3.0}% ", t),
                if t >= 1.0 { Style::default().fg(theme::level(t as f32)) } else { theme::faint() },
            )),
            (Some(_), None) => spans.push(Span::styled("thr   … ", theme::faint())),
            (None, _) => spans.push(Span::raw(" ".repeat(9))),
        }
        let mem_pct = match (c.mem_current, c.mem_max) {
            (Some(u), Some(m)) if m > 0 => Some(u as f32 / m as f32 * 100.0),
            _ => None,
        };
        spans.push(Span::styled("mem ", theme::dim()));
        bar_or_gap(&mut spans, bar_w, mem_pct);
        let used = c.mem_current.map_or("…".into(), fmt::bytes);
        let mem = match c.mem_max {
            Some(m) => format!("{used} / {}", fmt::bytes(m)),
            None => format!("{used} · no limit"),
        };
        spans.push(Span::styled(format!("{mem:<15}  "), theme::text()));
        let oom = c.oom_kills.unwrap_or(0);
        spans.push(Span::styled(
            format!("oom {oom}  "),
            if oom > 0 { Style::default().fg(p.bad).add_modifier(Modifier::BOLD) } else { theme::faint() },
        ));
        restarts(&mut spans, c);
        lines.push(Line::from(spans));
    }
    // Room left: the lesson this card is about.
    if lines.len() < INNER {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "limits are the cgroup's; the memory and cpu cards show the whole node",
            theme::faint(),
        )));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

const EIGHTHS: [&str; 9] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];

/// How the multi-line graphs are drawn; `D` cycles through them so they
/// can be compared on the same data.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GraphStyle {
    /// A bright braille line of the recent average, over a band from the
    /// recent minimum to maximum and a dim fill below, both cell
    /// backgrounds: smooth, keeps every peak, and needs no special font.
    Glow,
    /// rtop's own braille plot, coloured row by row.
    Braille,
    /// ratatui's `Chart` with `GraphType::Area` and braille dots.
    ChartBraille,
    /// The same chart with octants: braille resolution without the gaps
    /// between dots, but only in fonts with Unicode 16 glyphs.
    ChartOctant,
}

const STYLES: [GraphStyle; 4] =
    [GraphStyle::Glow, GraphStyle::Braille, GraphStyle::ChartBraille, GraphStyle::ChartOctant];
static STYLE: AtomicUsize = AtomicUsize::new(0);

pub fn graph_style() -> GraphStyle {
    STYLES[STYLE.load(Ordering::Relaxed) % STYLES.len()]
}

/// Switch to the next graph style and name it.
pub fn next_graph_style() -> &'static str {
    STYLE.store((STYLE.load(Ordering::Relaxed) + 1) % STYLES.len(), Ordering::Relaxed);
    match graph_style() {
        GraphStyle::Glow => "graphs: glow (line: average of 4 samples, band: their min–max)",
        GraphStyle::Braille => "graphs: rtop braille (coloured by height)",
        GraphStyle::ChartBraille => "graphs: ratatui Chart, area, braille marker",
        GraphStyle::ChartOctant => "graphs: ratatui Chart, area, octant marker (needs Unicode 16 glyphs)",
    }
}

/// A one-line graph of 0–100% samples, newest on the right, coloured by
/// value.
fn sparkline(values: &[f32], width: usize) -> Vec<Line<'static>> {
    plot(values, width, 1, |_, v| theme::level(v))
}

/// A graph of 0–100% samples filling `area`, newest on the right. Each row
/// is coloured by the height it represents, so peaks turn amber and red.
fn draw_graph(f: &mut Frame, area: Rect, values: &[f32]) {
    match graph_style() {
        GraphStyle::Glow => glow_plot(f.buffer_mut(), area, values, theme::level),
        GraphStyle::Braille => {
            let lines = plot(values, area.width as usize, area.height as usize, |top, _| theme::level(top));
            f.render_widget(Paragraph::new(lines), area);
        }
        style => draw_chart(f, area, values, None, style),
    }
}

/// Decades below the peak that a rate graph still shows.
const RATE_DECADES: f32 = 3.0;

/// A graph of rates with no natural maximum. The highest visible sample
/// fills the height, on a log scale: traffic spans orders of magnitude, and
/// on a linear one a single burst flattens everything else to nothing. Each
/// quarter of the height is a factor of about 5.6. Throughput isn't good or
/// bad, so one colour.
fn draw_rate_graph(f: &mut Frame, area: Rect, values: &[f32], color: Color) {
    let peak = visible(values, area.width as usize).iter().fold(0f32, |a, &b| a.max(b));
    let pct: Vec<f32> = values.iter().map(|&v| log_pct(v, peak)).collect();
    match graph_style() {
        GraphStyle::Glow => glow_plot(f.buffer_mut(), area, &pct, |_| color),
        GraphStyle::Braille => {
            let lines = plot(&pct, area.width as usize, area.height as usize, |_, _| color);
            f.render_widget(Paragraph::new(lines), area);
        }
        style => draw_chart(f, area, &pct, Some(color), style),
    }
}

/// Samples each point of the glow style summarises: the line is their
/// mean, the band spans their minimum to maximum.
const WINDOW: usize = 4;

/// The glow style, two points per cell like the braille plot. Raw samples
/// jitter, so drawing each one gives spiky noise; here the line follows the
/// mean of the last [`WINDOW`] samples and a band keeps their range, so no
/// peak is lost. The band is a halo in the 950 shade of the row's colour
/// (`color(row_top)`) as cell background, the line braille dots in the
/// colour itself. The newest point is drawn in
/// the brightest text colour.
fn glow_plot(buf: &mut Buffer, area: Rect, pct: &[f32], color: impl Fn(f32) -> Color) {
    let (w, h) = (area.width as usize, area.height as usize);
    if w == 0 || h == 0 {
        return;
    }
    let top = (h * 4 - 1) as f32;
    let dot = |v: f32| (v.clamp(0.0, 100.0) / 100.0 * top).round() as usize;
    // Per point: (line, band low, band high) as dot rows from the bottom.
    let start = pct.len().saturating_sub(w * 2);
    let mut points: Vec<Option<(usize, usize, usize)>> = vec![None; w * 2 - (pct.len() - start)];
    points.extend((start..pct.len()).map(|i| {
        let win = &pct[(i + 1).saturating_sub(WINDOW)..=i];
        let mean = win.iter().sum::<f32>() / win.len() as f32;
        let (lo, hi) = win.iter().fold((f32::MAX, f32::MIN), |(a, b), &v| (a.min(v), b.max(v)));
        Some((dot(mean), dot(lo), dot(hi)))
    }));
    // The line's span in each point's dot column: from the previous point's
    // level to its own, so the line stays connected through steep changes.
    let spans: Vec<Option<(usize, usize)>> = (0..points.len())
        .map(|i| {
            let (now, _, _) = points[i]?;
            let before = i.checked_sub(1).and_then(|j| points[j]).map_or(now, |p| p.0);
            Some((now.min(before), now.max(before)))
        })
        .collect();
    let band = 100.0 / h as f32;
    for c in 0..w {
        let x = area.x + c as u16;
        let pair = [points[c * 2], points[c * 2 + 1]];
        for from_bottom in 0..h {
            let y = area.y + (h - 1 - from_bottom) as u16;
            let (lo_dot, hi_dot) = (from_bottom * 4, from_bottom * 4 + 3);
            let mut bits = 0;
            for side in 0..2 {
                if let Some((a, b)) = spans[c * 2 + side] {
                    for d in a.max(lo_dot)..=b.min(hi_dot) {
                        bits |= DOTS[side][d - lo_dot];
                    }
                }
            }
            let in_band = pair.iter().flatten().any(|&(_, lo, hi)| lo <= hi_dot && hi >= lo_dot);
            let hue = color(band * (from_bottom + 1) as f32);
            let cell = &mut buf[(x, y)];
            if let Some(r) = theme::ramp(hue).filter(|_| in_band) {
                cell.set_bg(r[2]);
            }
            if bits != 0 {
                let fg = if c == w - 1 { theme::pal().bright } else { hue };
                cell.set_symbol(&char::from_u32(0x2800 + bits).unwrap_or(' ').to_string()).set_fg(fg);
            } else if in_band {
                cell.set_symbol(" ");
            }
        }
    }
}

/// ratatui's `Chart` as a filled area, two samples per cell like the
/// braille plot. A dataset has one colour and each dataset replaces whole
/// cells of the ones before, so stacked datasets per colour band leave gaps
/// at the band edges. Instead the series is drawn once and, with `color:
/// None`, each row is then recoloured by the height it represents.
fn draw_chart(f: &mut Frame, area: Rect, pct: &[f32], color: Option<Color>, style: GraphStyle) {
    let n = area.width as usize * 2;
    let shown = &pct[pct.len().saturating_sub(n)..];
    let start = (n - shown.len()) as f64;
    let data: Vec<(f64, f64)> =
        shown.iter().enumerate().map(|(i, &v)| (start + i as f64, v.clamp(0.0, 100.0) as f64)).collect();
    let marker = if style == GraphStyle::ChartOctant { Marker::Octant } else { Marker::Braille };
    let dataset = Dataset::default()
        .graph_type(GraphType::Area)
        .marker(marker)
        .style(Style::default().fg(color.unwrap_or(theme::pal().good)))
        .data(&data);
    let chart = Chart::new(vec![dataset])
        .x_axis(Axis::default().bounds([0.0, (n.max(2) - 1) as f64]))
        .y_axis(Axis::default().bounds([0.0, 100.0]));
    f.render_widget(chart, area);
    if color.is_none() {
        let h = area.height.max(1);
        let buf = f.buffer_mut();
        for row in 0..h {
            let fg = theme::level(100.0 * (h - row) as f32 / h as f32);
            for x in area.left()..area.right() {
                buf[(x, area.y + row)].set_fg(fg);
            }
        }
    }
}

/// `v` as 0–100 of a log scale from `peak / 10^RATE_DECADES` to `peak`.
/// Anything above zero gets at least a sliver, so activity always shows.
fn log_pct(v: f32, peak: f32) -> f32 {
    if v <= 0.0 || peak <= 0.0 {
        return 0.0;
    }
    let pct = ((v / peak).log10() / RATE_DECADES + 1.0) * 100.0;
    pct.clamp(1.0, 100.0)
}

/// The newest samples that fit `cells` columns: two per column for braille
/// graphs, one for a single-row sparkline.
fn visible(values: &[f32], cells: usize) -> &[f32] {
    &values[values.len().saturating_sub(cells * 2)..]
}

/// Braille dots of one cell column, bottom to top: left side, then right.
const DOTS: [[u32; 4]; 2] = [[0x40, 0x04, 0x02, 0x01], [0x80, 0x20, 0x10, 0x08]];

/// Draws 0–100 samples; `color(row_top, value)` picks each cell's colour.
/// Taller graphs use braille: two samples per cell and four dot rows per
/// line, filled from the bottom like an area chart. One line has too little
/// height for four steps, so it uses eighth blocks, one sample per cell.
fn plot(values: &[f32], width: usize, height: usize, color: impl Fn(f32, f32) -> Color) -> Vec<Line<'static>> {
    if height == 1 {
        let shown = &values[values.len().saturating_sub(width)..];
        let mut spans = vec![Span::raw(" ".repeat(width - shown.len()))];
        spans.extend(shown.iter().map(|&v| {
            let eighths = ((v.clamp(0.0, 100.0) / 100.0) * 8.0).round() as usize;
            if eighths == 0 {
                Span::styled("▁", theme::faint())
            } else {
                Span::styled(EIGHTHS[eighths], Style::default().fg(color(100.0, v)))
            }
        }));
        return vec![Line::from(spans)];
    }
    let shown = visible(values, width);
    // Pad on the left so the newest sample is always the right half of the
    // last cell; `None` is no sample yet.
    let mut cols: Vec<Option<f32>> = vec![None; width * 2 - shown.len()];
    cols.extend(shown.iter().map(|&v| Some(v)));
    let levels = (height * 4) as f32;
    let dots = |v: Option<f32>| v.map_or(0, |v| ((v.clamp(0.0, 100.0) / 100.0) * levels).round() as usize);
    let band = 100.0 / height as f32;
    (0..height)
        .map(|row| {
            let from_bottom = height - 1 - row;
            let lo = from_bottom as f32 * band;
            let spans = cols.chunks(2).map(|pair| {
                let mut bits = 0;
                for (side, v) in pair.iter().enumerate() {
                    let here = dots(*v).saturating_sub(from_bottom * 4).min(4);
                    bits |= DOTS[side][..here].iter().sum::<u32>();
                }
                let peak = pair.iter().flatten().fold(0f32, |a, &b| a.max(b));
                if bits == 0 && row == height - 1 && pair.iter().any(Option::is_some) {
                    // A quiet stretch still shows its baseline.
                    Span::styled("⣀", theme::faint())
                } else if bits == 0 {
                    Span::raw(" ")
                } else {
                    let ch = char::from_u32(0x2800 + bits).unwrap_or(' ');
                    Span::styled(ch.to_string(), Style::default().fg(color(lo + band, peak)))
                }
            });
            Line::from(spans.collect::<Vec<_>>())
        })
        .collect()
}

fn rate(bytes_per_sec: f32) -> String {
    format!("{}/s", fmt::bytes(bytes_per_sec as u64))
}

/// Two throughput graphs side by side (in | out, read | write), each scaled
/// to its own peak on a log scale. The title names where the counters come from.
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
        // The peak is the graph's top (log scale, see `rate_graph`); show
        // it when the half has room.
        let top = format!("max {}", rate(peak));
        if used + 2 + top.len() <= w {
            head.push(Span::raw(" ".repeat(w - used - top.len())));
            head.push(Span::styled(top, theme::faint()));
        }
        let [head_area, graph_area] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(half);
        f.render_widget(Paragraph::new(Line::from(head)), head_area);
        draw_rate_graph(f, graph_area, &values, color);
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
    let [name, graph_area] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(inner);
    f.render_widget(Paragraph::new(Span::styled(g.name.clone(), theme::dim())), name);
    draw_graph(f, graph_area, &hist);
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
    draw_graph(f, g, &hist);

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
    let title = if app.snapshot.containers.is_empty() { "memory" } else { "memory · node" };
    let block = panel(title)
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
    lines.extend(sparkline(&hist, w));
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
        Target::Node { context, node, detail } => {
            spans.push(Span::styled(context.clone(), ctx_style));
            if link.implicit_context {
                spans.push(Span::styled(" (current context)", Style::default().fg(p.warn)));
            }
            spans.extend([sep(), Span::styled(format!("node {node}"), theme::bold())]);
            if !detail.is_empty() {
                spans.extend([sep(), Span::styled(detail.clone(), theme::dim())]);
            }
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

#[cfg(test)]
mod graph_tests {
    use super::*;

    fn text(lines: &[Line]) -> Vec<String> {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect()
    }

    #[test]
    fn braille_puts_two_samples_in_a_cell_and_four_levels_in_a_line() {
        // 2 lines = 8 dot rows: 100% fills both, 50% the bottom line,
        // 25% half of it, 0% nothing; the left cell is the oldest pair.
        let got = text(&plot(&[100.0, 50.0, 25.0, 0.0], 2, 2, |_, _| Color::Reset));
        assert_eq!(got, ["⡇ ", "⣿⡄"]);
    }

    #[test]
    fn a_quiet_graph_shows_its_baseline_and_no_sample_shows_nothing() {
        let got = text(&plot(&[0.0, 0.0], 3, 2, |_, _| Color::Reset));
        assert_eq!(got, ["   ", "  ⣀"]);
    }

    #[test]
    fn glow_draws_the_mean_as_dots_over_a_min_max_band() {
        // One cell, 2 lines (8 dot rows, 0–7). The second point's window is
        // [0, 100]: mean 50% (dot 4, top line), range 0–7 (the band covers
        // both lines). The line climbs from the first point (dot 0).
        let area = Rect::new(0, 0, 1, 2);
        let mut buf = Buffer::empty(area);
        let good = theme::pal().good;
        glow_plot(&mut buf, area, &[0.0, 100.0], |_| good);
        let ramp = theme::ramp(good).expect("status colours are Tailwind shades");
        let (top, bottom) = (&buf[(0, 0)], &buf[(0, 1)]);
        assert_eq!((bottom.symbol(), bottom.bg), ("⣸", ramp[2]), "left: dot 0; right: climbs from 0 to 4");
        assert_eq!((top.symbol(), top.bg, top.fg), ("⢀", ramp[2], theme::pal().bright), "newest point is bright");
    }

    #[test]
    fn rates_are_log_scaled_below_the_peak() {
        assert_eq!(log_pct(1000.0, 1000.0), 100.0);
        assert!((log_pct(10.0, 1000.0) - 33.3).abs() < 0.1, "two decades down is a third");
        assert_eq!(log_pct(0.001, 1000.0), 1.0, "tiny but not zero still shows");
        assert_eq!(log_pct(0.0, 1000.0), 0.0);
    }
}
