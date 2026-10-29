//! Where process data comes from.
//!
//! Linux exposes processes as text files under `/proc`; macOS exposes them
//! through `proc_pidinfo(2)`-style syscalls that fill C structs. Both backends
//! produce the same [`Snapshot`]/[`ProcessDetails`] model.

pub mod cpu;
pub mod fixture;
pub mod linux_parse;

#[cfg(target_os = "macos")]
mod iokit;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

use std::collections::HashMap;

use crate::model::{Pid, ProcessDetails, Snapshot};

pub trait Collector {
    /// Human name of the platform, shown in the header.
    fn platform(&self) -> &'static str;
    /// Cheap per-tick listing of every process plus system totals.
    fn snapshot(&mut self) -> Snapshot;
    /// Everything we can learn about one process. Called when it is opened.
    fn details(&mut self, pid: Pid) -> anyhow::Result<ProcessDetails>;
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
