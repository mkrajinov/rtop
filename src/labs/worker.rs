//! The lab scenarios. Runs as `rtop __lab-worker <name>`, a fresh
//! single-threaded process, so calling fork() here is safe.
//!
//! Errors go to stderr, which rtop reads when the worker exits.

use std::fs::File;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use nix::unistd::{ForkResult, fork};

/// Labs never outlive this, even if rtop is killed without cleaning up.
const MAX_LIFETIME: Duration = Duration::from_secs(600);

pub fn run(name: &str) -> anyhow::Result<()> {
    let deadline = Instant::now() + MAX_LIFETIME;
    match name {
        "zombie" => zombie(deadline),
        "orphan" => orphan(deadline),
        "fdleak" => fdleak(deadline),
        "cpuhog" => cpuhog(),
        "memgrow" => memgrow(deadline),
        "threads" => threads(deadline),
        "pagecache" => pagecache(deadline),
        "pagecache-cold" => reader(true, deadline),
        "pagecache-warm" => reader(false, deadline),
        #[cfg(target_os = "linux")]
        "dstate" => dstate(deadline),
        #[cfg(target_os = "linux")]
        "namespace" => namespace(deadline),
        other => bail!("lab '{other}' is not available on this platform"),
    }
}

fn sleep_until(deadline: Instant) {
    while Instant::now() < deadline {
        thread::sleep(Duration::from_millis(500));
    }
}

/// Children must not keep rtop's stderr pipe open after the worker exits.
fn quiet() {
    if let Ok(null) = File::options().write(true).open("/dev/null") {
        use std::os::fd::AsRawFd;
        unsafe { libc::dup2(null.as_raw_fd(), 2) };
    }
}

/// fork(); the child runs `f` and exits without returning into the caller.
fn spawn_child(f: impl FnOnce()) -> anyhow::Result<libc::pid_t> {
    // SAFETY: the worker is single-threaded at every call site.
    match unsafe { fork() }.context("fork")? {
        ForkResult::Child => {
            quiet();
            f();
            unsafe { libc::_exit(0) }
        }
        ForkResult::Parent { child } => Ok(child.as_raw()),
    }
}

fn zombie(deadline: Instant) -> anyhow::Result<()> {
    for _ in 0..3 {
        // The child exits at once; we deliberately never wait() for it.
        spawn_child(|| {})?;
    }
    sleep_until(deadline);
    Ok(())
}

fn orphan(deadline: Instant) -> anyhow::Result<()> {
    spawn_child(|| sleep_until(deadline))?;
    thread::sleep(Duration::from_secs(15));
    Ok(())
}

fn fdleak(deadline: Instant) -> anyhow::Result<()> {
    let mut rl = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut rl) };
    rl.rlim_cur = 256.min(rl.rlim_max);
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &rl) } != 0 {
        bail!("setrlimit(RLIMIT_NOFILE): {}", std::io::Error::last_os_error());
    }
    let mut leaked = Vec::new();
    while Instant::now() < deadline {
        match File::open("/dev/null") {
            Ok(f) => leaked.push(f),
            // EMFILE: we hit the soft limit. Hold everything open and idle.
            Err(_) => break,
        }
        thread::sleep(Duration::from_millis(100));
    }
    sleep_until(deadline);
    drop(leaked);
    Ok(())
}

fn cpuhog() -> anyhow::Result<()> {
    let stop = Instant::now() + Duration::from_secs(120);
    let cores = thread::available_parallelism().map_or(2, |n| n.get());
    let mut kids = Vec::new();
    for nice in [0, 10] {
        for _ in 0..cores {
            kids.push(spawn_child(move || {
                unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, nice) };
                let mut x: u64 = 0;
                while Instant::now() < stop {
                    for _ in 0..100_000 {
                        x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
                    }
                }
            })?);
        }
    }
    for pid in kids {
        unsafe { libc::waitpid(pid, std::ptr::null_mut(), 0) };
    }
    Ok(())
}

fn memgrow(deadline: Instant) -> anyhow::Result<()> {
    const RESERVE: usize = 2 << 30;
    const TOUCH: usize = 1 << 30;
    const STEP: usize = 32 << 20;
    #[cfg(target_os = "linux")]
    let flags = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE;
    #[cfg(not(target_os = "linux"))]
    let flags = libc::MAP_PRIVATE | libc::MAP_ANON;
    let base = unsafe { libc::mmap(std::ptr::null_mut(), RESERVE, libc::PROT_READ | libc::PROT_WRITE, flags, -1, 0) };
    if base == libc::MAP_FAILED {
        bail!("mmap 2 GiB: {}", std::io::Error::last_os_error());
    }
    thread::sleep(Duration::from_secs(10));
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    let mut touched = 0;
    while touched < TOUCH && Instant::now() < deadline {
        for off in (touched..touched + STEP).step_by(page) {
            // SAFETY: inside the mapping we just created.
            unsafe { *(base as *mut u8).add(off) = 1 };
        }
        touched += STEP;
        thread::sleep(Duration::from_millis(500));
    }
    sleep_until(deadline);
    unsafe { libc::munmap(base, RESERVE) };
    Ok(())
}

/// Size of each pagecache lab file, and the pace both readers keep.
const PC_SIZE: usize = 256 << 20;
const PC_CHUNK: usize = 8 << 20;
const PC_RATE: f64 = 64.0 * (1 << 20) as f64;

/// Two files of the same size: one stays in the page cache, the other is
/// kept out of it. One reader each, started as `rtop __lab-worker
/// pagecache-cold|warm` so the COMMAND column says which is which. Each
/// reader gets its file as stdin; the files are unlinked at once, so the
/// space comes back when the lab's process group is killed.
fn pagecache(deadline: Instant) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let mut kids = Vec::new();
    for cold in [true, false] {
        let role = if cold { "cold" } else { "warm" };
        let path = std::env::temp_dir().join(format!("rtop-pagecache-{}-{role}", std::process::id()));
        let mut f = File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("create {}", path.display()))?;
        let _ = std::fs::remove_file(&path);
        if cold {
            no_cache(&f);
        }
        // Not zeros: some filesystems store all-zero blocks as holes.
        let chunk = vec![0xa5u8; PC_CHUNK];
        for _ in 0..PC_SIZE / PC_CHUNK {
            std::io::Write::write_all(&mut f, &chunk).context("write the lab file")?;
        }
        f.sync_all().context("fsync the lab file")?;
        if cold {
            evict(&f);
        }
        kids.push(
            std::process::Command::new(&exe)
                .args(["__lab-worker", &format!("pagecache-{role}")])
                .stdin(f)
                .stdout(std::process::Stdio::null())
                // Not rtop's stderr pipe: see quiet().
                .stderr(std::process::Stdio::null())
                .spawn()
                .context("start a reader")?,
        );
    }
    sleep_until(deadline);
    for mut k in kids {
        let _ = k.kill();
        let _ = k.wait();
    }
    Ok(())
}

/// Read stdin (the lab file) from start to end with pread() at
/// [`PC_RATE`], over and over, so every 1-second sample sees the reads.
fn reader(cold: bool, deadline: Instant) -> anyhow::Result<()> {
    use std::os::fd::FromRawFd;
    use std::os::unix::fs::FileExt;
    // SAFETY: fd 0 is the lab file pagecache() passed as stdin; nothing
    // else in this process uses stdin.
    let f = unsafe { File::from_raw_fd(0) };
    let size = f.metadata().context("fstat stdin")?.len() as usize;
    if cold {
        no_cache(&f);
    }
    let mut buf = vec![0u8; PC_CHUNK];
    let per_chunk = Duration::from_secs_f64(PC_CHUNK as f64 / PC_RATE);
    while Instant::now() < deadline {
        let mut off = 0;
        while off < size {
            let t = Instant::now();
            let n = f.read_at(&mut buf, off as u64).context("pread")?;
            if n == 0 {
                break;
            }
            off += n;
            thread::sleep(per_chunk.saturating_sub(t.elapsed()));
        }
        if cold {
            evict(&f);
        }
    }
    Ok(())
}

/// macOS: F_NOCACHE keeps this file's data out of the unified buffer cache.
/// Linux has no per-file switch short of O_DIRECT; [`evict`] drops it instead.
fn no_cache(_f: &File) {
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        unsafe { libc::fcntl(_f.as_raw_fd(), libc::F_NOCACHE, 1) };
    }
}

/// Linux: drop the file's clean pages from the page cache. Any user may do
/// this for a file they can read; dropping everything (drop_caches) needs root.
fn evict(_f: &File) {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        unsafe { libc::posix_fadvise(_f.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
    }
}

fn threads(deadline: Instant) -> anyhow::Result<()> {
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let busy = i % 2 == 0;
            thread::Builder::new().name(format!("{}-{i}", if busy { "busy" } else { "sleepy" })).spawn(move || {
                while Instant::now() < deadline {
                    if busy {
                        // ~20% duty cycle so the machine stays usable.
                        let t = Instant::now();
                        while t.elapsed() < Duration::from_millis(20) {
                            std::hint::black_box(0u64);
                        }
                        thread::sleep(Duration::from_millis(80));
                    } else {
                        thread::sleep(Duration::from_millis(500));
                    }
                }
            })
        })
        .collect::<Result<_, _>>()?;
    for h in handles {
        let _ = h.join();
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn dstate(deadline: Instant) -> anyhow::Result<()> {
    // vfork() semantics via clone(CLONE_VM | CLONE_VFORK): the parent is
    // suspended in the kernel until the child execs or exits. The child gets
    // its own stack, which keeps this sound in Rust (a bare vfork() would
    // share ours).
    extern "C" fn child(_: *mut libc::c_void) -> libc::c_int {
        unsafe {
            libc::sleep(60);
            libc::_exit(0)
        }
    }
    let mut stack = vec![0u8; 64 * 1024];
    while Instant::now() < deadline {
        let top = unsafe { stack.as_mut_ptr().add(stack.len()) };
        let top = (top as usize & !15) as *mut libc::c_void;
        let pid = unsafe {
            libc::clone(child, top, libc::CLONE_VM | libc::CLONE_VFORK | libc::SIGCHLD, std::ptr::null_mut())
        };
        if pid == -1 {
            bail!("clone(CLONE_VM|CLONE_VFORK): {}", std::io::Error::last_os_error());
        }
        unsafe { libc::waitpid(pid, std::ptr::null_mut(), 0) };
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn namespace(deadline: Instant) -> anyhow::Result<()> {
    let flags = libc::CLONE_NEWUSER | libc::CLONE_NEWPID | libc::CLONE_NEWUTS | libc::CLONE_NEWIPC | libc::CLONE_NEWNET;
    if unsafe { libc::unshare(flags) } != 0 {
        bail!(
            "unshare(NEWUSER|NEWPID|NEWUTS|NEWIPC|NEWNET): {} (needs unprivileged user namespaces, or run as root in the container)",
            std::io::Error::last_os_error()
        );
    }
    // CLONE_NEWPID applies to children: the next fork becomes PID 1 inside.
    let child = spawn_child(|| {
        let name = b"rtop-lab";
        unsafe { libc::sethostname(name.as_ptr().cast(), name.len()) };
        sleep_until(deadline);
    })?;
    unsafe { libc::waitpid(child, std::ptr::null_mut(), 0) };
    Ok(())
}
