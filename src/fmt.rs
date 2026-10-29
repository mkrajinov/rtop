//! Human-readable formatting shared by the collectors and the UI.

use std::time::{SystemTime, UNIX_EPOCH};

/// Binary units like `ps`/`top`: 1K = 1024 bytes.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "K", "M", "G", "T", "P"];
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    match unit {
        0 => format!("{n}B"),
        _ if v >= 100.0 => format!("{v:.0}{}", UNITS[unit]),
        _ => format!("{v:.1}{}", UNITS[unit]),
    }
}

/// `bytes` plus the exact value, for the detail view.
pub fn bytes_exact(n: u64) -> String {
    format!("{} ({n} bytes)", bytes(n))
}

/// htop's TIME+ column: `m:ss.cc`, or `h:mm:ss` once past an hour.
pub fn cpu_time(ns: u64) -> String {
    let centis = ns / 10_000_000;
    let secs = centis / 100;
    let (h, m, s) = (secs / 3600, (secs / 60) % 60, secs % 60);
    if h > 0 { format!("{h}h{m:02}:{s:02}") } else { format!("{m}:{s:02}.{:02}", centis % 100) }
}

pub fn uptime(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, (secs / 3600) % 24, (secs / 60) % 60);
    if d > 0 { format!("{d}d {h:02}:{m:02}") } else { format!("{h:02}:{m:02}:{:02}", secs % 60) }
}

/// "2h 05m ago" style age of a Unix timestamp.
pub fn age(start_unix: u64) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let s = now.saturating_sub(start_unix);
    match s {
        0..60 => format!("{s}s ago"),
        60..3600 => format!("{}m {:02}s ago", s / 60, s % 60),
        3600..86_400 => format!("{}h {:02}m ago", s / 3600, (s / 60) % 60),
        _ => format!("{}d {:02}h ago", s / 86_400, (s / 3600) % 24),
    }
}

pub fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(bytes(512), "512B");
        assert_eq!(bytes(4 << 20), "4.0M");
        assert_eq!(bytes(512 << 20), "512M");
        assert_eq!(cpu_time(61_500_000_000), "1:01.50");
        assert_eq!(cpu_time(3_723_000_000_000), "1h02:03");
        assert_eq!(uptime(93_784), "1d 02:03");
    }
}
