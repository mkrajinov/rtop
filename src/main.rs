use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::bail;
use clap::{Parser, Subcommand};
use crossterm::event::{self, Event, KeyEventKind};
use rtop::app::App;
use rtop::{collector, labs, ui};

#[derive(Parser)]
#[command(version, about = "A process explorer that teaches you what every number means")]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
    /// Refresh interval in seconds [default: 1.5, or 2 for rtop k8s].
    #[arg(short, long, global = true)]
    delay: Option<f32>,
    /// Colour theme (overrides the config file).
    #[arg(long, global = true, alias = "gray", value_parser = clap::builder::PossibleValuesParser::new(ui::theme::THEMES.iter().map(|t| t.name)))]
    theme: Option<String>,
    /// Accent colour (overrides the config file).
    #[arg(long, global = true, value_parser = clap::builder::PossibleValuesParser::new(ui::theme::ACCENTS.iter().map(|a| a.name)))]
    accent: Option<String>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Processes of a Kubernetes pod: pick a namespace and pod, or name one with --pod.
    K8s(K8sArgs),
    /// Start a guided lab: rtop spawns a misbehaving process tree and asks questions.
    Lab {
        /// Lab name; omit to list them.
        name: Option<String>,
    },
    /// Print everything rtop knows about one process, with sources, and exit.
    Inspect { pid: i32 },
    /// Internal: the process a lab inspects.
    #[command(name = "__lab-worker", hide = true)]
    LabWorker { name: String },
}

#[derive(clap::Args)]
struct K8sArgs {
    /// kubeconfig context to use [default: the current context].
    #[arg(long)]
    context: Option<String>,
    /// Open this pod directly instead of the picker.
    #[arg(long, value_name = "[NAMESPACE/]POD")]
    pod: Option<String>,
    /// Namespace of --pod, or the one the picker starts in [default: the context's namespace].
    #[arg(short = 'n', long)]
    namespace: Option<String>,
    /// Start narrowed to one container of the pod (C switches inside rtop).
    #[arg(short = 'c', long, requires = "pod")]
    container: Option<String>,
    /// Image for the debug container added to images without a shell.
    #[arg(long, value_name = "IMAGE", default_value = "busybox:1.37")]
    debug_image: String,
    /// Never add a debug container; containers without a shell are reported instead.
    #[arg(long)]
    no_debug_container: bool,
    /// Allow sending signals to processes in the pod (read-only otherwise).
    #[arg(long)]
    allow_actions: bool,
}

fn main() -> anyhow::Result<()> {
    let mut cli = Cli::parse();
    let cfg = rtop::config::load();
    ui::theme::set_by_name(cfg.theme.as_deref(), cfg.accent.as_deref());
    ui::theme::set_by_name(cli.theme.as_deref(), cli.accent.as_deref());
    let kube_mode = matches!(cli.command, Some(Cmd::K8s(_)));
    let delay = cli.delay.unwrap_or(if kube_mode { 2.0 } else { 1.5 }).max(0.2);
    let tick = Duration::from_secs_f32(delay);

    let mut lab = None;
    match cli.command.take() {
        Some(Cmd::K8s(args)) => return k8s(args, tick),
        Some(Cmd::LabWorker { name }) => return labs::worker::run(&name),
        Some(Cmd::Inspect { pid }) => return inspect(pid),
        Some(Cmd::Lab { name: None }) => {
            println!("Available labs:\n");
            for l in labs::LABS {
                let note = if labs::available(l) { "" } else { "  (Linux only)" };
                println!("  {:<10} {}{note}", l.name, l.title);
            }
            println!("\nRun: rtop lab <name>");
            return Ok(());
        }
        Some(Cmd::Lab { name: Some(name) }) => {
            let Some(l) = labs::find(&name) else { bail!("unknown lab '{name}' (run `rtop lab` to list)") };
            if !labs::available(l) {
                bail!("lab '{name}' needs Linux. Try it in the dev container: make linux-shell");
            }
            lab = Some(labs::LabSession::start(l)?);
        }
        None => {}
    }

    let mut app = App::new(collector::native());
    app.tick = tick;
    if let Some(l) = lab {
        app = app.with_lab(l);
    }

    install_quit_signals()?;
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut app);
    ratatui::restore();
    // Dropping the app kills the lab's process group.
    drop(app);
    Ok(result?)
}

/// Set from a signal handler; the event loop notices within one tick and
/// returns normally, so dropping `App` still kills the lab's process group.
static QUIT: AtomicBool = AtomicBool::new(false);

extern "C" fn on_quit_signal(_: libc::c_int) {
    QUIT.store(true, Ordering::Relaxed);
}

fn install_quit_signals() -> anyhow::Result<()> {
    use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
    let action = SigAction::new(SigHandler::Handler(on_quit_signal), SaFlags::empty(), SigSet::empty());
    for sig in [Signal::SIGTERM, Signal::SIGHUP, Signal::SIGINT] {
        // SAFETY: the handler only stores to an atomic.
        unsafe { sigaction(sig, &action)? };
    }
    Ok(())
}

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> io::Result<()> {
    app.refresh();
    let mut last = Instant::now();
    let mut dirty = true;
    while !app.quit && !app.back && !QUIT.load(Ordering::Relaxed) {
        if dirty {
            terminal.draw(|f| ui::draw(f, app))?;
            dirty = false;
        }
        // Scrolling a trackpad in the alternate screen arrives as hundreds of
        // arrow keys a second. Handle everything already queued, then draw
        // once: drawing per key made rtop the busiest process on the machine.
        let mut timeout = app.tick.saturating_sub(last.elapsed());
        while event::poll(timeout)? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    app.handle_key(key);
                    dirty = true;
                }
                Event::Resize(..) => dirty = true,
                _ => {}
            }
            timeout = Duration::ZERO;
        }
        if last.elapsed() >= app.tick {
            app.refresh();
            last = Instant::now();
            dirty = true;
        }
    }
    Ok(())
}

/// `rtop k8s`: the picker, and the pod view opened from it. Esc in the pod
/// view comes back to the picker.
///
/// The client is built, and a pod named with `--pod` is opened, before the
/// TUI starts, so those errors print as plain one-line messages
/// (contracts/cli.md). Pods chosen in the picker are opened on a thread
/// while the picker keeps drawing, and their errors show in its status line.
#[cfg(feature = "kube")]
fn k8s(args: K8sArgs, tick: Duration) -> anyhow::Result<()> {
    use collector::kube::KubeCollector;
    use rtop::picker::Picker;

    eprintln!("connecting to {} …", args.context.as_deref().unwrap_or("the current context"));
    let (pod_ns, pod) = match args.pod.as_deref().map(|p| p.split_once('/')) {
        Some(Some((ns, name))) => (Some(ns.to_string()), Some(name.to_string())),
        Some(None) => (args.namespace.clone(), args.pod.clone()),
        None => (args.namespace.clone(), None),
    };
    let mut picker = Picker::connect(args.context.clone(), pod_ns.clone())?;
    let first = match pod {
        Some(pod) => Some(KubeCollector::connect(pod_options(&args, &picker, picker.ns.clone(), pod, tick))?),
        None => None,
    };

    install_quit_signals()?;
    let mut terminal = ratatui::init();
    let result = k8s_loop(&mut terminal, &mut picker, first, &args, tick);
    ratatui::restore();
    Ok(result?)
}

/// Both pod views use the context the picker resolved, so a kubeconfig
/// changed meanwhile cannot send rtop to another cluster.
#[cfg(feature = "kube")]
fn pod_options(
    args: &K8sArgs,
    picker: &rtop::picker::Picker,
    namespace: String,
    pod: String,
    tick: Duration,
) -> collector::kube::PodOptions {
    collector::kube::PodOptions {
        context: Some(picker.context.clone()),
        implicit_context: picker.implicit_context,
        namespace: Some(namespace),
        pod,
        tick,
        debug_image: args.debug_image.clone(),
        debug: !args.no_debug_container,
    }
}

#[cfg(feature = "kube")]
fn k8s_loop(
    terminal: &mut ratatui::DefaultTerminal,
    picker: &mut rtop::picker::Picker,
    mut next: Option<collector::kube::KubeCollector>,
    args: &K8sArgs,
    tick: Duration,
) -> io::Result<()> {
    let mut container = args.container.clone();
    loop {
        let collector = match next.take() {
            Some(c) => c,
            None => match pick(terminal, picker, args, tick)? {
                Some(c) => c,
                None => return Ok(()),
            },
        };
        let mut app = App::new(Box::new(collector));
        app.tick = tick;
        app.container = container.take();
        app.allow_actions = args.allow_actions;
        app.can_go_back = true;
        run(terminal, &mut app)?;
        if !app.back {
            return Ok(());
        }
        if let Some(rtop::model::Target::Pod { namespace, pod, .. }) = app.collector.target() {
            picker.came_back(namespace, pod);
        }
    }
}

/// Run the picker until a pod view is open (`None`: quit).
#[cfg(feature = "kube")]
fn pick(
    terminal: &mut ratatui::DefaultTerminal,
    picker: &mut rtop::picker::Picker,
    args: &K8sArgs,
    tick: Duration,
) -> io::Result<Option<collector::kube::KubeCollector>> {
    use std::sync::mpsc;

    use collector::kube::KubeCollector;
    use crossterm::event::KeyCode;
    use rtop::picker::Action;

    let mut opening: Option<mpsc::Receiver<anyhow::Result<KubeCollector>>> = None;
    while !QUIT.load(Ordering::Relaxed) {
        picker.tick();
        if let Some(rx) = &opening {
            match rx.try_recv() {
                Ok(Ok(c)) => {
                    picker.connecting = None;
                    return Ok(Some(c));
                }
                Ok(Err(e)) => {
                    picker.connecting = None;
                    picker.set_status(e.to_string());
                    opening = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    picker.connecting = None;
                    picker.set_status("opening the pod failed");
                    opening = None;
                }
            }
        }
        terminal.draw(|f| ui::picker::draw(f, picker))?;
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let Event::Key(key) = event::read()? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if opening.is_some() && key.code == KeyCode::Esc {
            // The thread finishes on its own; what it opened is dropped,
            // which closes its readers.
            opening = None;
            picker.connecting = None;
            picker.set_status("cancelled");
            continue;
        }
        match picker.handle_key(key) {
            Action::Quit => return Ok(None),
            Action::Open { ns, pod } => {
                picker.connecting = Some(format!("{ns}/{pod}"));
                let opts = pod_options(args, picker, ns, pod, tick);
                let (tx, rx) = mpsc::channel();
                std::thread::spawn(move || {
                    let _ = tx.send(KubeCollector::connect(opts));
                });
                opening = Some(rx);
            }
            Action::None => {}
        }
    }
    Ok(None)
}

#[cfg(not(feature = "kube"))]
fn k8s(_: K8sArgs, _: Duration) -> anyhow::Result<()> {
    bail!("this rtop was built without Kubernetes support: rebuild with default features (cargo build --release)")
}

fn inspect(pid: i32) -> anyhow::Result<()> {
    let mut c = collector::native();
    let d = c.details(rtop::model::ProcId::local(pid))?;
    println!("PID {pid} · {} ({})", d.name, c.platform());
    for n in &d.notes {
        println!("! {n}");
    }
    let section = |title: &str, fields: &[rtop::model::Field]| {
        println!("\n== {title}");
        for f in fields {
            println!("{:<34} {:<40} ← {}", f.label, f.value, f.source);
        }
    };
    section("Overview", &d.overview);
    section("Memory", &d.memory);
    section("Limits", &d.limits);
    for (title, fields) in &d.kernel.sections {
        section(title, fields);
    }
    println!("\n== Files ({})", d.files.len());
    for f in &d.files {
        println!("{:>5}  {:<8} {}", f.fd, f.kind, f.target);
    }
    println!("\n== Threads ({})", d.threads.len());
    for t in &d.threads {
        println!("{:>12}  {:<20} {:<24} {:?}", t.tid, t.name, t.state, t.cpu_time_ns.map(rtop::fmt::cpu_time));
    }
    println!("\n== Memory map: {} regions", d.regions.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Cli, Cmd};

    fn parse(args: &str) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(args.split_whitespace())
    }

    #[test]
    fn kubernetes_flags_live_under_k8s() {
        assert!(parse("rtop --pod multi").is_err());
        assert!(parse("rtop --context kind-rtop").is_err());
        let Ok(cli) = parse("rtop k8s --context kind-rtop -n rtop-demo -d 1") else { panic!() };
        let Some(Cmd::K8s(k)) = cli.command else { panic!() };
        assert_eq!((k.context.as_deref(), k.namespace.as_deref(), k.pod), (Some("kind-rtop"), Some("rtop-demo"), None));
        assert_eq!(cli.delay, Some(1.0));
        assert!(parse("rtop k8s --pod rtop-demo/multi -c app --allow-actions").is_ok());
        // A container only makes sense for a named pod.
        assert!(parse("rtop k8s -c app").is_err());
    }
}
