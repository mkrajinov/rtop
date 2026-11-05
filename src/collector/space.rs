//! Disk space: the arithmetic shared by both platforms, and how macOS APFS
//! volumes are grouped. Pure, so it is tested everywhere.

use crate::model::{Volume, Volumes};

/// Size, used and free bytes the way df(1) computes them from statvfs:
/// used counts the root reserve's blocks as not in use, free does not
/// include them.
pub fn space(blocks: u64, bfree: u64, bavail: u64, frsize: u64) -> (u64, u64, u64) {
    (blocks * frsize, blocks.saturating_sub(bfree) * frsize, bavail * frsize)
}

/// One `struct statfs` from getfsstat(2).
#[derive(Clone, Debug, Default)]
pub struct FsEntry {
    pub mount: String,
    pub device: String,
    pub fstype: String,
    pub blocks: u64,
    pub bfree: u64,
    pub bavail: u64,
    pub bsize: u64,
    /// Not flagged `MNT_DONTBROWSE` ("nobrowse" in mount(8)): Finder shows it.
    pub browse: bool,
}

/// `/dev/disk3s1s1` → `disk3`: the APFS container (a synthesized disk) the
/// volume lives in. All its volumes share the container's free space.
fn container(device: &str) -> Option<&str> {
    let name = device.strip_prefix("/dev/")?;
    let digits = name.strip_prefix("disk")?;
    let end = digits.find(|c: char| !c.is_ascii_digit()).unwrap_or(digits.len());
    (end > 0).then(|| &name[..4 + end])
}

/// One volume per APFS container (the space is shared, so per-volume rows
/// would each claim the same free space), and one per other filesystem.
/// statfs reports the container's blocks and free blocks for every APFS
/// volume, so `blocks - bfree` is what the whole container uses: Finder's
/// figure, not df's per-volume Used. A container is shown on the dashboard
/// when one of its volumes is browsable, under that volume's mount point.
pub fn group(entries: Vec<FsEntry>) -> Vec<Volume> {
    let mut groups: Vec<(String, Vec<FsEntry>)> = Vec::new();
    for e in entries {
        if e.blocks == 0 || crate::collector::linux_parse::PSEUDO_FS.contains(&e.fstype.as_str()) {
            continue;
        }
        let key = match (e.fstype.as_str(), container(&e.device)) {
            ("apfs", Some(c)) => c.to_string(),
            _ => e.device.clone(),
        };
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, g)) => g.push(e),
            None => groups.push((key, vec![e])),
        }
    }
    groups
        .into_iter()
        .map(|(key, g)| {
            let browsable: Vec<&FsEntry> = g.iter().filter(|e| e.browse).collect();
            let pick = |list: &[&FsEntry]| list.iter().min_by_key(|e| e.mount.len()).map(|e| (*e).clone());
            let all: Vec<&FsEntry> = g.iter().collect();
            let main = pick(&browsable).or_else(|| pick(&all)).expect("a group has an entry");
            let (size, used, free) = space(main.blocks, main.bfree, main.bavail, main.bsize);
            let note = if g.len() > 1 {
                let mut names: Vec<&str> = g.iter().map(|e| e.mount.as_str()).collect();
                names.sort_by_key(|n| n.len());
                Some(format!("APFS container {key}: {} volumes share this space ({})", g.len(), names.join(", ")))
            } else if browsable.is_empty() {
                Some("hidden by the system (nobrowse)".into())
            } else {
                None
            };
            Volume {
                mount: main.mount,
                device: if g.len() > 1 { key } else { main.device },
                fstype: main.fstype,
                size,
                used,
                free,
                shown: !browsable.is_empty(),
                note,
            }
        })
        .collect()
}

/// Volumes, re-read at most every `every`: statfs is cheap, but the list
/// rarely changes and its numbers move slowly.
pub struct Cache {
    at: Option<std::time::Instant>,
    last: Option<Volumes>,
}

impl Cache {
    pub const EVERY: std::time::Duration = std::time::Duration::from_secs(5);

    pub fn new() -> Self {
        Cache { at: None, last: None }
    }

    pub fn get(&mut self, read: impl FnOnce() -> Option<Volumes>) -> Option<Volumes> {
        if self.at.is_none_or(|t| t.elapsed() >= Self::EVERY) {
            self.last = read();
            self.at = Some(std::time::Instant::now());
        }
        self.last.clone()
    }
}

impl Default for Cache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fs(mount: &str, device: &str, fstype: &str, browse: bool, blocks: u64, bfree: u64) -> FsEntry {
        FsEntry {
            mount: mount.into(),
            device: device.into(),
            fstype: fstype.into(),
            blocks,
            bfree,
            bavail: bfree,
            bsize: 4096,
            browse,
        }
    }

    #[test]
    fn apfs_volumes_are_one_container_named_by_the_browsable_one() {
        // `df` on this Mac: `/` 43% used, but the container is 98% full
        // because the user's files are on the nobrowse Data volume.
        let c = |m: &str, d: &str, b| fs(m, d, "apfs", b, 1000, 20);
        let v = group(vec![
            c("/", "/dev/disk3s1s1", true),
            fs("/dev", "devfs", "devfs", false, 281, 0),
            c("/System/Volumes/VM", "/dev/disk3s6", false),
            c("/System/Volumes/Data", "/dev/disk3s5", false),
            fs("/System/Volumes/xarts", "/dev/disk1s2", "apfs", false, 100, 90),
            fs("/System/Volumes/Data/home", "map auto_home", "autofs", false, 0, 0),
            fs("/Volumes/USB", "/dev/disk4s1", "msdos", true, 500, 250),
        ]);
        let rows: Vec<_> =
            v.iter().map(|v| (v.mount.as_str(), v.device.as_str(), v.shown, v.percent().round())).collect();
        assert_eq!(
            rows,
            [
                ("/", "disk3", true, 98.0),
                ("/System/Volumes/xarts", "/dev/disk1s2", false, 10.0),
                ("/Volumes/USB", "/dev/disk4s1", true, 50.0)
            ]
        );
        assert!(v[0].note.as_deref().unwrap().contains("3 volumes share"));
    }

    #[test]
    fn container_name() {
        assert_eq!(container("/dev/disk3s1s1"), Some("disk3"));
        assert_eq!(container("/dev/disk12s2"), Some("disk12"));
        assert_eq!(container("map auto_home"), None);
    }
}
