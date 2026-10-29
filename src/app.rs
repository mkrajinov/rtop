//! Application state and input handling. Rendering lives in `ui`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nix::sys::signal::Signal;

use crate::collector::cpu::{CoreTracker, CpuTracker, RateTracker};
use crate::collector::{self, Collector};
use crate::explain::Catalog;
use crate::labs::LabSession;
use crate::model::*;
use crate::ui::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    Pid,
    /// Only shown for a Kubernetes target.
    Container,
    User,
    State,
    Pri,
    Nice,
    Cpu,
    Mem,
    Vsz,
    Rss,
    Threads,
    Fds,
    Time,
    Command,
}

impl Column {
    pub const ALL: [Column; 14] = [
        Column::Pid,
        Column::Container,
        Column::User,
        Column::State,
        Column::Pri,
        Column::Nice,
        Column::Cpu,
        Column::Mem,
        Column::Vsz,
        Column::Rss,
        Column::Threads,
        Column::Fds,
        Column::Time,
        Column::Command,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Column::Pid => "PID",
            Column::Container => "CONTAINER",
            Column::User => "USER",
            Column::State => "S",
            Column::Pri => "PRI",
            Column::Nice => "NI",
            Column::Cpu => "CPU%",
            Column::Mem => "MEM%",
            Column::Vsz => "VSZ",
            Column::Rss => "RSS",
            Column::Threads => "THR",
            Column::Fds => "FD",
            Column::Time => "TIME+",
            Column::Command => "COMMAND",
        }
    }

    /// Catalog entry explaining this column.
    pub fn key(self) -> &'static str {
        match self {
            Column::Pid => "pid",
            Column::Container => "container",
            Column::User => "uid",
            Column::State => "state",
            Column::Pri => "priority",
            Column::Nice => "nice",
            Column::Cpu => "cpu_percent",
            Column::Mem => "mem_percent",
            Column::Vsz => "vsz",
            Column::Rss => "rss",
            Column::Threads => "threads",
            Column::Fds => "fds",
            Column::Time => "cpu_time",
            Column::Command => "cmdline",
        }
    }

    /// Numeric columns sort largest-first by default.
    fn default_desc(self) -> bool {
        !matches!(self, Column::Pid | Column::Container | Column::User | Column::Command | Column::State)
    }

    fn cmp(self, a: &Process, b: &Process) -> std::cmp::Ordering {
        match self {
            Column::Pid => a.pid.cmp(&b.pid),
            Column::Container => container(a).cmp(container(b)),
            Column::User => a.user.cmp(&b.user),
            Column::State => a.state.letter().cmp(&b.state.letter()),
            Column::Pri => a.priority.cmp(&b.priority),
            Column::Nice => a.nice.cmp(&b.nice),
            Column::Cpu => a.cpu_percent.total_cmp(&b.cpu_percent),
            Column::Mem | Column::Rss => a.rss.cmp(&b.rss),
            Column::Vsz => a.vsz.cmp(&b.vsz),
            Column::Threads => a.threads.cmp(&b.threads),
            Column::Fds => a.fds.cmp(&b.fds),
            Column::Time => a.cpu_time_ns.cmp(&b.cpu_time_ns),
            Column::Command => a.cmdline.cmp(&b.cmdline),
        }
        .then(a.pid.cmp(&b.pid))
    }
}

/// The container a process runs in; empty in local mode.
pub fn container(p: &Process) -> &str {
    p.origin.as_ref().map_or("", |o| &o.container)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Memory,
    Files,
    Threads,
    Env,
    Limits,
    Kernel,
    Raw,
}

impl Tab {
    pub const ALL: [Tab; 8] =
        [Tab::Overview, Tab::Memory, Tab::Files, Tab::Threads, Tab::Env, Tab::Limits, Tab::Kernel, Tab::Raw];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Memory => "Memory",
            Tab::Files => "Files",
            Tab::Threads => "Threads",
            Tab::Env => "Env",
            Tab::Limits => "Limits",
            Tab::Kernel => "Kernel",
            Tab::Raw => "Raw",
        }
    }
}

/// One selectable line of a detail tab.
pub enum DetailRow<'a> {
    Header(&'a str),
    Field(&'a Field),
    Region(&'a Region),
    File(&'a OpenFile),
    Thread(&'a ThreadInfo),
    Env(&'a str, &'a str),
    Raw { text: &'a str, title: bool },
    Text(&'a str),
}

impl DetailRow<'_> {
    /// Which catalog entry the explain panel shows for this row.
    pub fn key(&self, tab: Tab) -> &'static str {
        match self {
            DetailRow::Field(f) => f.key,
            DetailRow::Region(_) => "maps",
            DetailRow::File(_) => "fds",
            DetailRow::Thread(_) => "threads",
            DetailRow::Env(..) => "environ",
            DetailRow::Raw { .. } => "raw",
            DetailRow::Header(_) | DetailRow::Text(_) => match tab {
                Tab::Kernel => "namespaces",
                Tab::Limits => "limits",
                Tab::Memory => "maps",
                Tab::Raw => "raw",
                _ => "pid",
            },
        }
    }
}

pub struct DetailState {
    pub pid: ProcId,
    pub data: Result<ProcessDetails, String>,
    pub tab: Tab,
    pub selected: usize,
    pub offset: usize,
    /// Highlight raw lines containing this text (set by `r` on a field).
    pub raw_hint: Option<String>,
}

impl DetailState {
    pub fn rows(&self) -> Vec<DetailRow<'_>> {
        let Ok(d) = &self.data else { return Vec::new() };
        let mut rows = Vec::new();
        match self.tab {
            Tab::Overview => rows.extend(d.overview.iter().map(DetailRow::Field)),
            Tab::Memory => {
                rows.extend(d.memory.iter().map(DetailRow::Field));
                if !d.regions.is_empty() {
                    rows.push(DetailRow::Header("Memory map (address range · perms · kind · resident · path)"));
                    rows.extend(d.regions.iter().map(DetailRow::Region));
                }
            }
            Tab::Files => rows.extend(d.files.iter().map(DetailRow::File)),
            Tab::Threads => rows.extend(d.threads.iter().map(DetailRow::Thread)),
            Tab::Env => rows.extend(d.env.iter().map(|(k, v)| DetailRow::Env(k, v))),
            Tab::Limits => rows.extend(d.limits.iter().map(DetailRow::Field)),
            Tab::Kernel => {
                if let Some(n) = &d.kernel.note {
                    rows.push(DetailRow::Text(n));
                }
                for (title, fields) in &d.kernel.sections {
                    rows.push(DetailRow::Header(title));
                    rows.extend(fields.iter().map(DetailRow::Field));
                }
            }
            Tab::Raw => {
                for src in &d.raw {
                    rows.push(DetailRow::Raw { text: &src.title, title: true });
                    rows.extend(src.body.lines().map(|l| DetailRow::Raw { text: l, title: false }));
                    rows.push(DetailRow::Raw { text: "", title: false });
                }
            }
        }
        rows
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Table,
    Detail,
    Filter,
    Signal,
    Help,
    Theme,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    All,
    /// A process and its descendants.
    Family(ProcId),
}

pub struct Row {
    pub idx: usize,
    pub prefix: String,
}

pub const SIGNALS: &[(Signal, &str)] = &[
    (Signal::SIGTERM, "polite stop request; the process may catch it and clean up (kill's default)"),
    (Signal::SIGINT, "what Ctrl-C sends; usually 'stop what you are doing'"),
    (Signal::SIGHUP, "terminal hung up; many daemons reload their config on it"),
    (
        Signal::SIGKILL,
        "cannot be caught or ignored; the kernel removes the process (unless it is in D state or a zombie)",
    ),
    (Signal::SIGSTOP, "freeze the process (state T); cannot be caught"),
    (Signal::SIGCONT, "resume a stopped process"),
    (Signal::SIGQUIT, "like INT but asks for a core dump (Ctrl-\\)"),
    (Signal::SIGUSR1, "application-defined (e.g. dd prints progress, nginx reopens logs)"),
    (Signal::SIGUSR2, "application-defined"),
];

pub struct App {
    pub collector: Box<dyn Collector>,
    pub catalog: Catalog,
    cpu: CpuTracker,
    cores: CoreTracker,
    pub core_usage: Vec<f32>,
    /// Average CPU% and memory% over time, newest last, for the header graphs.
    pub cpu_hist: VecDeque<f32>,
    pub mem_hist: VecDeque<f32>,
    net_rate: RateTracker,
    disk_rate: RateTracker,
    /// Bytes/second over time: [received, sent] and [read, written].
    pub net_hist: [VecDeque<f32>; 2],
    pub disk_hist: [VecDeque<f32>; 2],
    pub gpu_hist: VecDeque<f32>,
    /// Show the executable's directory in COMMAND (toggled with `c`).
    pub show_path: bool,
    pub snapshot: Snapshot,
    pub rows: Vec<Row>,
    pub selected: usize,
    /// First visible table row, kept between frames so scrolling is stable.
    pub table_offset: usize,
    selected_pid: Option<ProcId>,
    pub sort: Column,
    pub sort_desc: bool,
    pub focus: Column,
    pub filter: String,
    pub tree: bool,
    pub scope: Scope,
    pub paused: bool,
    pub show_explain: bool,
    pub explain_scroll: u16,
    pub mode: Mode,
    pub detail: Option<DetailState>,
    pub signal_sel: usize,
    /// Theme picker: which list has focus (0 theme, 1 accent) and the theme
    /// to restore on Esc.
    pub theme_focus: usize,
    theme_before: (usize, usize),
    pub status: Option<(String, Instant)>,
    pub lab: Option<LabSession>,
    /// Kubernetes mode: show only this container's processes (`-c`, `C`).
    pub container: Option<String>,
    /// Kubernetes mode: signals are off unless rtop was started with
    /// `--allow-actions` (FR-011).
    pub allow_actions: bool,
    pub tick: Duration,
    pub quit: bool,
}

impl App {
    pub fn new(collector: Box<dyn Collector>) -> Self {
        App {
            collector,
            catalog: Catalog::load(),
            cpu: CpuTracker::default(),
            cores: CoreTracker::default(),
            core_usage: Vec::new(),
            cpu_hist: VecDeque::new(),
            mem_hist: VecDeque::new(),
            net_rate: RateTracker::default(),
            disk_rate: RateTracker::default(),
            net_hist: Default::default(),
            disk_hist: Default::default(),
            gpu_hist: VecDeque::new(),
            show_path: false,
            snapshot: Snapshot::default(),
            rows: Vec::new(),
            selected: 0,
            table_offset: 0,
            selected_pid: None,
            sort: Column::Cpu,
            sort_desc: true,
            focus: Column::Cpu,
            filter: String::new(),
            tree: false,
            scope: Scope::All,
            paused: false,
            show_explain: false,
            explain_scroll: 0,
            mode: Mode::Table,
            detail: None,
            signal_sel: 0,
            theme_focus: 0,
            theme_before: (0, 0),
            status: None,
            lab: None,
            container: None,
            allow_actions: false,
            tick: Duration::from_millis(1500),
            quit: false,
        }
    }

    pub fn with_lab(mut self, lab: LabSession) -> Self {
        self.lab = Some(lab);
        self.tree = true;
        self.sort = Column::Pid;
        self.sort_desc = false;
        self
    }

    pub fn refresh(&mut self) {
        if let Some(lab) = self.lab.as_mut() {
            lab.poll();
        }
        if self.paused {
            return;
        }
        let mut snap = self.collector.snapshot();
        // A remote collector hands out the same sample until a new one
        // arrives; computing CPU% again would read as 0% for everything.
        if snap.clock.is_some() && snap.clock == self.snapshot.clock {
            if self.detail.is_some() {
                self.reload_detail();
            }
            return;
        }
        let now = snap.clock.unwrap_or_else(Instant::now);
        self.cpu.update(&mut snap.processes, now);
        self.core_usage = self.cores.update(&snap.system.cores);
        let sys = &snap.system;
        let avg = self.core_usage.iter().sum::<f32>() / self.core_usage.len().max(1) as f32;
        let mem = sys.mem_used as f32 / sys.mem_total.max(1) as f32 * 100.0;
        let [rx, tx] = sys.net.as_ref().map_or([0.0; 2], |c| self.net_rate.update(c, now));
        let [rd, wr] = sys.disk.as_ref().map_or([0.0; 2], |c| self.disk_rate.update(c, now));
        let gpu = sys.gpu.as_ref().map_or(0.0, |g| g.busy);
        let [net_rx, net_tx] = &mut self.net_hist;
        let [disk_rd, disk_wr] = &mut self.disk_hist;
        for (hist, v) in [
            (&mut self.cpu_hist, avg),
            (&mut self.mem_hist, mem),
            (net_rx, rx),
            (net_tx, tx),
            (disk_rd, rd),
            (disk_wr, wr),
            (&mut self.gpu_hist, gpu),
        ] {
            hist.push_back(v);
            if hist.len() > 400 {
                hist.pop_front();
            }
        }
        self.snapshot = snap;
        self.rebuild();
        if self.detail.is_some() {
            self.reload_detail();
        }
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Instant::now()));
    }

    pub fn status_text(&self) -> Option<&str> {
        self.status.as_ref().filter(|(_, t)| t.elapsed() < Duration::from_secs(6)).map(|(s, _)| s.as_str())
    }

    pub fn selected_process(&self) -> Option<&Process> {
        self.rows.get(self.selected).map(|r| &self.snapshot.processes[r.idx])
    }

    pub fn mem_percent(&self, p: &Process) -> f32 {
        let total = self.snapshot.system.mem_total.max(1);
        (p.rss as f64 / total as f64 * 100.0) as f32
    }

    /// Recompute visible rows: scope, filter, sort and tree layout.
    pub fn rebuild(&mut self) {
        let procs = &self.snapshot.processes;
        // Parents are looked up inside the same PID namespace (`space`).
        let by_pid: HashMap<ProcId, usize> = procs.iter().enumerate().map(|(i, p)| (p.id(), i)).collect();
        let mut children: HashMap<ProcId, Vec<usize>> = HashMap::new();
        for (i, p) in procs.iter().enumerate() {
            if p.ppid != p.pid {
                children.entry(p.parent_id()).or_default().push(i);
            }
        }

        let mut scope: Option<HashSet<ProcId>> = match self.scope {
            Scope::All => None,
            Scope::Family(root) => {
                let mut set = HashSet::new();
                let mut stack = vec![root];
                while let Some(pid) = stack.pop() {
                    if set.insert(pid) {
                        stack.extend(children.get(&pid).into_iter().flatten().map(|&i| procs[i].id()));
                    }
                }
                Some(set)
            }
        };
        if let Some(lab) = &self.lab {
            let lab_set: HashSet<ProcId> = procs.iter().filter(|p| lab.in_scope(p)).map(|p| p.id()).collect();
            scope = Some(match scope {
                Some(s) => s.intersection(&lab_set).copied().collect(),
                None => lab_set,
            });
        }

        let needle = self.filter.to_lowercase();
        let only = self.container.as_deref();
        let matches = |p: &Process| {
            only.is_none_or(|c| container(p) == c)
                && (needle.is_empty()
                    || p.pid.to_string() == needle
                    || p.cmdline.to_lowercase().contains(&needle)
                    || p.name.to_lowercase().contains(&needle)
                    || p.user.to_lowercase().contains(&needle)
                    || container(p).to_lowercase().contains(&needle))
        };
        let mut keep: HashSet<ProcId> = procs
            .iter()
            .filter(|p| scope.as_ref().is_none_or(|s| s.contains(&p.id())) && matches(p))
            .map(|p| p.id())
            .collect();

        let (sort, desc) = (self.sort, self.sort_desc);
        let order = |a: &usize, b: &usize| {
            let o = sort.cmp(&procs[*a], &procs[*b]);
            if desc { o.reverse() } else { o }
        };

        let mut rows = Vec::with_capacity(keep.len());
        if self.tree {
            // Keep ancestors of matches so the tree stays connected.
            if !needle.is_empty() {
                for pid in keep.clone() {
                    let mut cur = pid;
                    while let Some(&i) = by_pid.get(&cur) {
                        let pp = procs[i].parent_id();
                        if pp == cur || !by_pid.contains_key(&pp) || scope.as_ref().is_some_and(|s| !s.contains(&pp)) {
                            break;
                        }
                        keep.insert(pp);
                        cur = pp;
                    }
                }
            }
            let mut roots: Vec<usize> = procs
                .iter()
                .enumerate()
                .filter(|(_, p)| keep.contains(&p.id()) && (p.ppid == p.pid || !keep.contains(&p.parent_id())))
                .map(|(i, _)| i)
                .collect();
            roots.sort_by(order);
            // Depth-first walk; the stack holds (process, indent so far, is last sibling, depth).
            let mut stack: Vec<(usize, String, bool, usize)> =
                roots.iter().rev().map(|&r| (r, String::new(), true, 0)).collect();
            while let Some((i, prefix, last, depth)) = stack.pop() {
                let branch = match (depth, last) {
                    (0, _) => "",
                    (_, true) => "└─ ",
                    _ => "├─ ",
                };
                rows.push(Row { idx: i, prefix: format!("{prefix}{branch}") });
                let mut kids: Vec<usize> = children
                    .get(&procs[i].id())
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|&k| keep.contains(&procs[k].id()))
                    .collect();
                kids.sort_by(order);
                let next = match (depth, last) {
                    (0, _) => String::new(),
                    (_, true) => format!("{prefix}   "),
                    _ => format!("{prefix}│  "),
                };
                let n = kids.len();
                stack.extend(kids.into_iter().enumerate().rev().map(|(j, k)| (k, next.clone(), j + 1 == n, depth + 1)));
            }
        } else {
            let mut idx: Vec<usize> = (0..procs.len()).filter(|&i| keep.contains(&procs[i].id())).collect();
            idx.sort_by(order);
            rows.extend(idx.into_iter().map(|idx| Row { idx, prefix: String::new() }));
        }
        self.rows = rows;

        // Once the user picks a process, keep it selected as rows reorder.
        // Until then (or after Home) the cursor stays on the top row.
        if let Some(pid) = self.selected_pid {
            match self.rows.iter().position(|r| self.snapshot.processes[r.idx].id() == pid) {
                Some(pos) => self.selected = pos,
                None => self.selected_pid = None,
            }
        }
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
    }

    fn select(&mut self, i: usize) {
        self.selected = i.min(self.rows.len().saturating_sub(1));
        self.selected_pid = self.selected_process().map(Process::id);
        self.explain_scroll = 0;
    }

    fn open_detail(&mut self) {
        let Some(pid) = self.selected_process().map(Process::id) else { return };
        self.detail = Some(DetailState {
            pid,
            data: self.collector.details(pid).map_err(|e| e.to_string()),
            tab: Tab::Overview,
            selected: 0,
            offset: 0,
            raw_hint: None,
        });
        self.mode = Mode::Detail;
        self.explain_scroll = 0;
    }

    fn reload_detail(&mut self) {
        if let Some(d) = self.detail.as_mut() {
            match self.collector.details(d.pid) {
                Ok(data) => d.data = Ok(data),
                // A remote answer is on its way; keep showing what we have.
                Err(e) if collector::is_pending(&e) => {}
                // Keep the last good data on screen and say what happened.
                Err(e) if d.data.is_ok() => self.status = Some((format!("PID {}: {e}", d.pid), Instant::now())),
                Err(e) => d.data = Err(e.to_string()),
            }
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        match self.mode {
            Mode::Filter => self.key_filter(key),
            Mode::Signal => self.key_signal(key),
            Mode::Help => self.mode = self.base_mode(),
            Mode::Theme => self.key_theme(key),
            Mode::Detail => self.key_detail(key),
            Mode::Table => self.key_table(key),
        }
    }

    fn key_common(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('e') => {
                self.show_explain = !self.show_explain;
                self.explain_scroll = 0;
            }
            KeyCode::Char('T') => {
                self.theme_before = theme::current();
                self.mode = Mode::Theme;
            }
            KeyCode::Char(']') => self.explain_scroll = self.explain_scroll.saturating_add(3),
            KeyCode::Char('[') => self.explain_scroll = self.explain_scroll.saturating_sub(3),
            KeyCode::Char('?') | KeyCode::F(1) => self.mode = Mode::Help,
            KeyCode::Char('p') => {
                self.paused = !self.paused;
                self.set_status(if self.paused { "paused — the snapshot is frozen" } else { "resumed" });
            }
            KeyCode::Char('n') if self.lab.is_some() => self.lab.as_mut().unwrap().next(),
            KeyCode::Char('b') if self.lab.is_some() => self.lab.as_mut().unwrap().prev(),
            KeyCode::Char('v') if self.lab.is_some() => {
                let lab = self.lab.as_mut().unwrap();
                lab.revealed = !lab.revealed;
            }
            _ => return false,
        }
        true
    }

    fn base_mode(&self) -> Mode {
        if self.detail.is_some() { Mode::Detail } else { Mode::Table }
    }

    /// Arrow keys apply a theme live; Enter saves it, Esc restores the old one.
    fn key_theme(&mut self, key: KeyEvent) {
        let (g, a) = theme::current();
        let step = |i: usize, n: usize, up: bool| if up { (i + n - 1) % n } else { (i + 1) % n };
        match key.code {
            KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => self.theme_focus ^= 1,
            KeyCode::Up | KeyCode::Down if self.theme_focus == 0 => {
                theme::set(step(g, theme::THEMES.len(), key.code == KeyCode::Up), a)
            }
            KeyCode::Up | KeyCode::Down => theme::set(g, step(a, theme::ACCENTS.len(), key.code == KeyCode::Up)),
            KeyCode::Esc | KeyCode::Char('q') => {
                theme::set(self.theme_before.0, self.theme_before.1);
                self.mode = self.base_mode();
            }
            KeyCode::Enter => {
                let (name, accent) = theme::names();
                let cfg = crate::config::Config { theme: Some(name.into()), accent: Some(accent.into()) };
                let msg = match crate::config::save(&cfg) {
                    Ok(p) => format!("theme {name} + {accent} saved to {}", p.display()),
                    Err(e) => format!("theme applied, but saving failed: {e}"),
                };
                self.set_status(msg);
                self.mode = self.base_mode();
            }
            _ => {}
        }
    }

    fn key_table(&mut self, key: KeyEvent) {
        if self.key_common(key) {
            return;
        }
        let page = 20;
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc => {
                if !self.filter.is_empty() || self.scope != Scope::All {
                    self.filter.clear();
                    self.scope = Scope::All;
                    self.rebuild();
                }
            }
            KeyCode::Up => self.select(self.selected.saturating_sub(1)),
            KeyCode::Down => self.select(self.selected + 1),
            KeyCode::PageUp => self.select(self.selected.saturating_sub(page)),
            KeyCode::PageDown => self.select(self.selected + page),
            KeyCode::Home => {
                self.select(0);
                self.selected_pid = None;
            }
            KeyCode::End => self.select(usize::MAX),
            KeyCode::Left => self.move_focus(-1),
            KeyCode::Right => self.move_focus(1),
            KeyCode::Char('s') | KeyCode::F(6) => self.sort_by(self.focus),
            KeyCode::Char('<') => {
                self.move_focus(-1);
                self.sort_by(self.focus);
            }
            KeyCode::Char('>') => {
                self.move_focus(1);
                self.sort_by(self.focus);
            }
            KeyCode::Char('/') => self.mode = Mode::Filter,
            KeyCode::Char('c') => self.show_path = !self.show_path,
            KeyCode::Char('t') => {
                self.tree = !self.tree;
                self.rebuild();
            }
            KeyCode::Char('f') => {
                self.scope = match (self.scope, self.selected_process()) {
                    (Scope::All, Some(p)) => Scope::Family(p.id()),
                    _ => Scope::All,
                };
                if self.scope != Scope::All {
                    self.tree = true;
                }
                self.rebuild();
            }
            KeyCode::Char('C') if self.collector.target().is_some() => self.next_container(),
            KeyCode::Char('k') if self.selected_process().is_some() => {
                if !self.remote_blocked() {
                    self.signal_sel = 0;
                    self.mode = Mode::Signal;
                }
            }
            KeyCode::Char('+') | KeyCode::Char('=') => self.renice(1),
            KeyCode::Char('-') => self.renice(-1),
            KeyCode::Enter => self.open_detail(),
            _ => {}
        }
    }

    /// The table's columns: CONTAINER only when reading a Kubernetes target.
    pub fn columns(&self) -> Vec<Column> {
        let remote = self.collector.target().is_some();
        Column::ALL.into_iter().filter(|&c| remote || c != Column::Container).collect()
    }

    /// `C`: all containers → the first → the next … → all again.
    fn next_container(&mut self) {
        let mut names: Vec<&str> = self.snapshot.processes.iter().map(container).filter(|c| !c.is_empty()).collect();
        names.sort();
        names.dedup();
        let next = match self.container.as_deref() {
            None => names.first(),
            Some(cur) => names.iter().skip_while(|&&n| n != cur).nth(1),
        };
        self.container = next.map(|n| n.to_string());
        match &self.container {
            Some(c) => self.set_status(format!("container {c} only · C for the next one")),
            None => self.set_status("all containers"),
        }
        self.rebuild();
    }

    /// In Kubernetes mode a PID means nothing on this machine: never pass
    /// it to the local kill(2) or setpriority(2).
    fn remote_blocked(&mut self) -> bool {
        if self.collector.target().is_none() {
            return false;
        }
        if !self.allow_actions {
            self.set_status("read-only: start rtop with --allow-actions to send signals in Kubernetes mode");
            return true;
        }
        false
    }

    fn move_focus(&mut self, delta: isize) {
        let cols = self.columns();
        let i = cols.iter().position(|c| *c == self.focus).unwrap_or(0) as isize;
        let n = cols.len() as isize;
        self.focus = cols[((i + delta).rem_euclid(n)) as usize];
        self.explain_scroll = 0;
    }

    fn sort_by(&mut self, col: Column) {
        if self.sort == col {
            self.sort_desc = !self.sort_desc;
        } else {
            self.sort = col;
            self.sort_desc = col.default_desc();
        }
        self.rebuild();
    }

    fn key_filter(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.filter.clear();
                self.mode = Mode::Table;
            }
            KeyCode::Enter => self.mode = Mode::Table,
            KeyCode::Backspace => {
                self.filter.pop();
            }
            KeyCode::Char(c) => self.filter.push(c),
            _ => {}
        }
        self.rebuild();
    }

    fn key_signal(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Table,
            KeyCode::Up => self.signal_sel = self.signal_sel.saturating_sub(1),
            KeyCode::Down => self.signal_sel = (self.signal_sel + 1).min(SIGNALS.len() - 1),
            KeyCode::Enter => {
                self.mode = Mode::Table;
                if let Some(p) = self.selected_process() {
                    let (pid, name) = (p.pid, p.name.clone());
                    let sig = SIGNALS[self.signal_sel].0;
                    if self.collector.target().is_some() {
                        let (id, where_) = (p.id(), container(p).to_string());
                        let msg = match self.collector.signal(id, sig.as_str().trim_start_matches("SIG")) {
                            Ok(()) => format!("kill -s {sig} {pid} sent in container {where_}"),
                            Err(e) => format!("kill {pid} in {where_}: {e}"),
                        };
                        self.set_status(msg);
                        return;
                    }
                    let msg = match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), sig) {
                        Ok(()) => format!("kill({pid}, {sig}) sent to {name}"),
                        Err(nix::errno::Errno::EPERM) => format!(
                            "kill({pid}, {sig}): EPERM — you may only signal processes whose real or effective UID matches yours (root may signal anyone)"
                        ),
                        Err(nix::errno::Errno::ESRCH) => {
                            format!("kill({pid}, {sig}): ESRCH — no such process (it already exited)")
                        }
                        Err(e) => format!("kill({pid}, {sig}): {e}"),
                    };
                    self.set_status(msg);
                }
            }
            _ => {}
        }
    }

    fn renice(&mut self, delta: i32) {
        if self.remote_blocked() {
            return;
        }
        if self.collector.target().is_some() {
            self.set_status("renice is not available in Kubernetes mode yet");
            return;
        }
        let Some(p) = self.selected_process() else { return };
        let (pid, new) = (p.pid, (p.nice + delta).clamp(-20, 19));
        // setpriority(2) returns -1 with errno on failure.
        let rc = unsafe { libc::setpriority(libc::PRIO_PROCESS, pid as libc::id_t, new) };
        let msg = if rc == 0 {
            format!("setpriority(PRIO_PROCESS, {pid}, {new}) ok — nice is now {new}")
        } else {
            let err = std::io::Error::last_os_error();
            match err.raw_os_error() {
                Some(libc::EACCES) | Some(libc::EPERM) if delta < 0 => format!(
                    "setpriority → {err}: lowering nice (more CPU) needs root{}",
                    if cfg!(target_os = "linux") { " or CAP_SYS_NICE" } else { "" }
                ),
                Some(libc::EPERM) => format!("setpriority → {err}: not your process"),
                _ => format!("setpriority({pid}, {new}) → {err}"),
            }
        };
        self.set_status(msg);
    }

    fn key_detail(&mut self, key: KeyEvent) {
        if self.key_common(key) {
            return;
        }
        let Some(d) = self.detail.as_mut() else {
            self.mode = Mode::Table;
            return;
        };
        let n = d.rows().len();
        let tab_idx = Tab::ALL.iter().position(|t| *t == d.tab).unwrap_or(0);
        let switch = |d: &mut DetailState, t: Tab| {
            d.tab = t;
            d.selected = 0;
            d.offset = 0;
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Backspace => {
                self.detail = None;
                self.mode = Mode::Table;
                self.explain_scroll = 0;
                return;
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => switch(d, Tab::ALL[(tab_idx + 1) % Tab::ALL.len()]),
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                switch(d, Tab::ALL[(tab_idx + Tab::ALL.len() - 1) % Tab::ALL.len()])
            }
            KeyCode::Char(c @ '1'..='8') => switch(d, Tab::ALL[c as usize - '1' as usize]),
            KeyCode::Up | KeyCode::Char('k') => d.selected = d.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => d.selected = (d.selected + 1).min(n.saturating_sub(1)),
            KeyCode::PageUp => d.selected = d.selected.saturating_sub(20),
            KeyCode::PageDown => d.selected = (d.selected + 20).min(n.saturating_sub(1)),
            KeyCode::Home | KeyCode::Char('g') => d.selected = 0,
            KeyCode::End | KeyCode::Char('G') => d.selected = n.saturating_sub(1),
            KeyCode::Char('r') => {
                let hint = match d.rows().get(d.selected) {
                    Some(DetailRow::Field(f)) => f.raw_hint.clone(),
                    _ => None,
                };
                d.tab = Tab::Raw;
                d.selected = 0;
                if let Some(h) = &hint
                    && let Some(pos) = d
                        .rows()
                        .iter()
                        .position(|r| matches!(r, DetailRow::Raw { text, title: false } if text.contains(h.as_str())))
                {
                    d.selected = pos;
                }
                d.raw_hint = hint;
            }
            _ => {}
        }
        self.explain_scroll = 0;
    }

    /// Key for the explain panel in the current context.
    pub fn explain_key(&self) -> &'static str {
        match (&self.mode, &self.detail) {
            (_, Some(d)) => d.rows().get(d.selected).map_or("pid", |r| r.key(d.tab)),
            _ => self.focus.key(),
        }
    }
}
