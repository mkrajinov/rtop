//! CPU% is not something the kernel stores. It only keeps a running total of
//! CPU time per process (and per core); a percentage is the *difference*
//! between two samples divided by the wall-clock time between them. That is
//! why every top-like tool shows 0% on its first frame.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::model::{CpuTicks, IoCounters, ProcId, Process};

/// 100% means one full core. A process with 4 busy threads can show 400%.
pub fn percent(prev_ns: u64, now_ns: u64, elapsed: Duration) -> f32 {
    let wall = elapsed.as_nanos() as f64;
    if wall <= 0.0 {
        return 0.0;
    }
    (now_ns.saturating_sub(prev_ns) as f64 / wall * 100.0) as f32
}

#[derive(Default)]
pub struct CpuTracker {
    /// Keyed by (process, start time) so a recycled PID never inherits old totals.
    prev: HashMap<(ProcId, u64), u64>,
    last: Option<Instant>,
}

impl CpuTracker {
    pub fn update(&mut self, procs: &mut [Process], now: Instant) {
        let elapsed = self.last.map(|t| now.duration_since(t));
        let mut next = HashMap::with_capacity(procs.len());
        for p in procs.iter_mut() {
            let key = (p.id(), p.start_time);
            p.cpu_percent = match (elapsed, self.prev.get(&key)) {
                (Some(e), Some(&prev)) => percent(prev, p.cpu_time_ns, e),
                _ => 0.0,
            };
            next.insert(key, p.cpu_time_ns);
        }
        self.prev = next;
        self.last = Some(now);
    }
}

/// Per-core utilisation: busy delta / total delta between two samples.
#[derive(Default)]
pub struct CoreTracker {
    prev: Vec<CpuTicks>,
}

impl CoreTracker {
    pub fn update(&mut self, cores: &[CpuTicks]) -> Vec<f32> {
        let usage = cores
            .iter()
            .enumerate()
            .map(|(i, now)| match self.prev.get(i) {
                Some(prev) if now.total > prev.total => {
                    let busy = now.busy.saturating_sub(prev.busy) as f64;
                    (busy / (now.total - prev.total) as f64 * 100.0) as f32
                }
                _ => 0.0,
            })
            .collect();
        self.prev = cores.to_vec();
        usage
    }
}

/// Bytes per second from two samples of a pair of byte counters (network
/// in/out, disk read/write), the same delta-over-time idea as CPU%.
#[derive(Default)]
pub struct RateTracker {
    prev: Option<(u64, u64, Instant)>,
}

impl RateTracker {
    /// (read, write) bytes/second; zero on the first sample. A counter that
    /// went backwards (an interface or disk disappeared) also reads as zero.
    pub fn update(&mut self, c: &IoCounters, now: Instant) -> [f32; 2] {
        let rate = self.prev.map_or([0.0; 2], |(r, w, t)| {
            let secs = now.duration_since(t).as_secs_f64().max(1e-3);
            [(c.read.saturating_sub(r) as f64 / secs) as f32, (c.write.saturating_sub(w) as f64 / secs) as f32]
        });
        self.prev = Some((c.read, c.write, now));
        rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collector::fixture::process;

    #[test]
    fn percent_is_delta_over_wall_time() {
        let half_second = 500_000_000;
        assert_eq!(percent(0, half_second, Duration::from_secs(1)), 50.0);
        assert_eq!(percent(0, 4_000_000_000, Duration::from_secs(1)), 400.0);
        assert_eq!(percent(10, 5, Duration::from_secs(1)), 0.0);
        assert_eq!(percent(0, 1, Duration::ZERO), 0.0);
    }

    #[test]
    fn first_sample_is_zero_and_recycled_pid_restarts() {
        let mut t = CpuTracker::default();
        let start = Instant::now();
        let mut procs = vec![process(1, 0, "a")];
        procs[0].cpu_time_ns = 1_000_000_000;
        t.update(&mut procs, start);
        assert_eq!(procs[0].cpu_percent, 0.0);

        procs[0].cpu_time_ns = 1_250_000_000;
        t.update(&mut procs, start + Duration::from_secs(1));
        assert_eq!(procs[0].cpu_percent, 25.0);

        // Same PID, different start time: a new process, no history.
        procs[0].start_time += 10;
        procs[0].cpu_time_ns = 9_000_000_000;
        t.update(&mut procs, start + Duration::from_secs(2));
        assert_eq!(procs[0].cpu_percent, 0.0);
    }

    #[test]
    fn core_usage() {
        let mut t = CoreTracker::default();
        assert_eq!(t.update(&[CpuTicks { busy: 10, total: 100 }]), vec![0.0]);
        assert_eq!(t.update(&[CpuTicks { busy: 40, total: 200 }]), vec![30.0]);
    }

    #[test]
    fn rate_is_bytes_per_second() {
        let mut r = RateTracker::default();
        let t = Instant::now();
        let c = |read, write| IoCounters { read, write, source: String::new() };
        assert_eq!(r.update(&c(1000, 0), t), [0.0, 0.0]);
        assert_eq!(r.update(&c(3000, 500), t + Duration::from_secs(2)), [1000.0, 250.0]);
        assert_eq!(r.update(&c(10, 500), t + Duration::from_secs(3)), [0.0, 0.0]);
    }
}
