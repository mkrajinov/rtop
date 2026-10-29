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
    /// Refresh interval in seconds.
    #[arg(short, long, default_value_t = 1.5)]
    delay: f32,
    /// Colour theme (overrides the config file).
    #[arg(long, alias = "gray", value_parser = clap::builder::PossibleValuesParser::new(ui::theme::THEMES.iter().map(|t| t.name)))]
    theme: Option<String>,
    /// Accent colour (overrides the config file).
    #[arg(long, value_parser = clap::builder::PossibleValuesParser::new(ui::theme::ACCENTS.iter().map(|a| a.name)))]
    accent: Option<String>,
}

#[derive(Subcommand)]
enum Cmd {
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

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let mut lab = None;
    match cli.command {
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

    let cfg = rtop::config::load();
    ui::theme::set_by_name(cfg.theme.as_deref(), cfg.accent.as_deref());
    ui::theme::set_by_name(cli.theme.as_deref(), cli.accent.as_deref());

    let mut app = App::new(collector::native());
    app.tick = std::time::Duration::from_secs_f32(cli.delay.max(0.2));
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
    while !app.quit && !QUIT.load(Ordering::Relaxed) {
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
