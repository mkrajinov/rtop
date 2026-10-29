//! Where process data comes from.
//!
//! Linux exposes processes as text files under `/proc`; macOS exposes them
//! through `proc_pidinfo(2)`-style syscalls that fill C structs. Both backends
//! produce the same [`Snapshot`]/[`ProcessDetails`] model.

pub mod cpu;
pub mod fixture;
#[cfg(feature = "kube")]
pub mod kube;
pub mod linux_parse;

#[cfg(target_os = "macos")]
mod iokit;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

use std::collections::HashMap;

use crate::model::{Link, ProcId, ProcessDetails, Snapshot, Target};

pub trait Collector {
    /// Human name of the platform, shown in the header.
    fn platform(&self) -> &'static str;
    /// Cheap per-tick listing of every process plus system totals.
    fn snapshot(&mut self) -> Snapshot;
    /// Everything we can learn about one process. Called when it is opened
    /// and again on every refresh while it stays open. A remote collector
    /// returns [`Pending`] until the answer arrives instead of blocking.
    fn details(&mut self, id: ProcId) -> anyhow::Result<ProcessDetails>;
    /// The Kubernetes pod or node being read; `None` for this machine.
    fn target(&self) -> Option<&Target> {
        None
    }
    /// Health of the connection to the target.
    fn link(&self) -> Option<Link> {
        None
    }
    /// Send a signal (`kill -s <name>`) inside a remote target. Local
    /// collectors leave this to the app, which calls kill(2) directly.
    fn signal(&mut self, _id: ProcId, _signal: &str) -> anyhow::Result<()> {
        anyhow::bail!("not supported here")
    }
}

/// `details()` has been requested but the answer is still on its way.
#[derive(Debug)]
pub struct Pending;

impl std::fmt::Display for Pending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("loading…")
    }
}

impl std::error::Error for Pending {}

pub fn is_pending(e: &anyhow::Error) -> bool {
    e.is::<Pending>()
}

#[cfg(target_os = "linux")]
pub fn native() -> Box<dyn Collector> {
    Box::new(linux::LinuxCollector::new())
}

#[cfg(target_os = "macos")]
pub fn native() -> Box<dyn Collector> {
    Box::new(macos::MacCollector::new())
}

/// uid -> user name, resolved through `getpwuid_r(3)` once per uid.
#[derive(Default)]
pub struct UserCache(HashMap<u32, String>);

impl UserCache {
    pub fn name(&mut self, uid: u32) -> String {
        self.0
            .entry(uid)
            .or_insert_with(|| {
                nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid))
                    .ok()
                    .flatten()
                    .map(|u| u.name)
                    .unwrap_or_else(|| uid.to_string())
            })
            .clone()
    }
}
