//! The byte stream the `sh` reader prints inside a pod or node
//! (specs/001-k8s-targets/contracts/reader-wire.md).
//!
//! Each answer is a run of frames closed by a done marker:
//!
//! ```text
//! \x1eF <name>\n <payload> \x1eE <rc>\n   one file or record
//! \x1eD <seq>\n                           end of the answer to request <seq>
//! ```
//!
//! `\x1e` (ASCII record separator) never occurs in `/proc` text files.
//! `cmdline` and `environ` hold arbitrary bytes; if one contains `\x1eE `,
//! that frame ends early and the rest is skipped. The command shows
//! truncated, nothing else breaks.
//!
//! Pure functions only, so this is tested on every OS.

use crate::model::Pid;

const RS: u8 = 0x1e;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub name: String,
    pub body: Vec<u8>,
    /// Exit status of the read: 0 when the file was readable.
    pub rc: i32,
}

impl Frame {
    pub fn ok(&self) -> bool {
        self.rc == 0
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// Everything the reader printed for one request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Response {
    pub seq: u64,
    pub frames: Vec<Frame>,
}

impl Response {
    /// The frame with this exact name, if it was readable.
    pub fn get(&self, name: &str) -> Option<&Frame> {
        self.frames.iter().find(|f| f.name == name && f.ok())
    }

    pub fn text(&self, name: &str) -> Option<String> {
        self.get(name).map(Frame::text)
    }

    /// `p <pid>` records of a `snap` answer.
    pub fn procs(&self) -> impl Iterator<Item = ProcRecord> + '_ {
        self.frames.iter().filter(|f| f.ok()).filter_map(ProcRecord::parse)
    }
}

/// One process from a `snap`: the `stat` line, the real UID from `status`
/// (`None` when status was unreadable) and the number of open fds (`None`
/// when `fd/` was unreadable).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcRecord {
    pub pid: Pid,
    pub stat: String,
    pub uid: Option<u32>,
    pub fds: Option<u32>,
}

impl ProcRecord {
    fn parse(f: &Frame) -> Option<Self> {
        let pid = f.name.strip_prefix("p ")?.parse().ok()?;
        let text = f.text();
        // uid and fd count are the last two lines; the comm inside the stat
        // line may itself contain a newline, so split from the end.
        let mut parts = text.trim_end_matches('\n').rsplitn(3, '\n');
        let fds: i64 = parts.next()?.trim().parse().ok()?;
        let uid: i64 = parts.next()?.trim().parse().ok()?;
        let stat = parts.next()?.to_string();
        Some(ProcRecord { pid, stat, uid: u32::try_from(uid).ok(), fds: u32::try_from(fds).ok() })
    }
}

/// `L <fd> <target>` lines of a `fd` frame, as `ls -l` would show them.
pub fn links(body: &str) -> Vec<(i32, String)> {
    let mut v: Vec<(i32, String)> = body
        .lines()
        .filter_map(|l| {
            let mut it = l.strip_prefix("L ")?.splitn(2, ' ');
            Some((it.next()?.parse().ok()?, it.next().unwrap_or("").to_string()))
        })
        .collect();
    v.sort();
    v
}

/// `L <name> <target>` lines with any name, e.g. the `ns` frame.
pub fn named_links(body: &str) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = body
        .lines()
        .filter_map(|l| {
            let mut it = l.strip_prefix("L ")?.splitn(2, ' ');
            Some((it.next()?.to_string(), it.next().unwrap_or("").to_string()))
        })
        .collect();
    v.sort();
    v
}

/// Collects bytes as they arrive from the exec stream and hands out whole
/// answers. A partial answer stays buffered until its done marker arrives.
#[derive(Default)]
pub struct Decoder {
    buf: Vec<u8>,
}

impl Decoder {
    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// The next complete answer, if one has fully arrived.
    pub fn next_response(&mut self) -> Option<Response> {
        let (seq, end) = find_done(&self.buf)?;
        let resp = Response { seq, frames: frames(&self.buf[..end.0]) };
        self.buf.drain(..end.1);
        Some(resp)
    }
}

/// Position of the first `\x1eD <seq>\n`: (seq, (start, end)).
fn find_done(buf: &[u8]) -> Option<(u64, (usize, usize))> {
    let mut from = 0;
    while let Some(i) = find(&buf[from..], &[RS, b'D', b' ']).map(|i| i + from) {
        let rest = &buf[i + 3..];
        let nl = rest.iter().position(|&b| b == b'\n')?;
        if let Some(seq) = std::str::from_utf8(&rest[..nl]).ok().and_then(|s| s.trim().parse().ok()) {
            return Some((seq, (i, i + 3 + nl + 1)));
        }
        from = i + 3;
    }
    None
}

fn frames(mut b: &[u8]) -> Vec<Frame> {
    let mut out = Vec::new();
    while let Some(start) = find(b, &[RS, b'F', b' ']) {
        b = &b[start + 3..];
        let Some(nl) = b.iter().position(|&c| c == b'\n') else { break };
        let name = String::from_utf8_lossy(&b[..nl]).into_owned();
        b = &b[nl + 1..];
        let Some(end) = find(b, &[RS, b'E', b' ']) else { break };
        let body = b[..end].to_vec();
        b = &b[end + 3..];
        let nl = b.iter().position(|&c| c == b'\n').unwrap_or(b.len());
        // Not a number: a stray `\x1eE ` inside the payload cut the frame short.
        // The read itself worked, so keep the truncated body as readable.
        let rc = std::str::from_utf8(&b[..nl]).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
        b = &b[(nl + 1).min(b.len())..];
        out.push(Frame { name, body, rc });
    }
    out
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!("{}/tests/fixtures/kube/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    fn one(bytes: &[u8]) -> Response {
        let mut d = Decoder::default();
        d.push(bytes);
        let r = d.next_response().expect("a complete answer");
        assert!(d.next_response().is_none());
        r
    }

    #[test]
    fn hello_has_passwd_and_pagesize() {
        let r = one(&fixture("hello.bin"));
        assert_eq!(r.seq, 0);
        assert_eq!(r.text("version").as_deref(), Some("1\n"));
        assert!(r.text("passwd").unwrap().starts_with("root:x:0:0"));
        assert_eq!(r.text("pagesize").unwrap().trim(), "4096");
    }

    #[test]
    fn snap_has_system_files_and_processes() {
        let r = one(&fixture("snap-pod.bin"));
        assert_eq!(r.seq, 1);
        assert!(r.text("/proc/stat").unwrap().starts_with("cpu "));
        assert!(r.get("/proc/meminfo").is_some());
        let procs: Vec<_> = r.procs().collect();
        assert_eq!(procs.len(), 2);
        assert_eq!(procs[0].pid, 1);
        assert!(procs[0].stat.starts_with("1 (sleep) S 0"));
        assert_eq!((procs[0].uid, procs[0].fds), (Some(0), Some(3)));
        // fd/ of the second process was unreadable.
        assert_eq!((procs[1].pid, procs[1].uid, procs[1].fds), (7, Some(1000), None));
    }

    #[test]
    fn unreadable_file_is_kept_but_not_returned() {
        let r = one(&fixture("details.bin"));
        let environ = r.frames.iter().find(|f| f.name == "environ").unwrap();
        assert_ne!(environ.rc, 0);
        assert!(r.get("environ").is_none());
        assert!(r.text("status").unwrap().contains("Name:\tsleep"));
    }

    #[test]
    fn fd_links() {
        let r = one(&fixture("details.bin"));
        let fds = links(&r.text("fd").unwrap());
        assert_eq!(fds[0], (0, "/dev/null".into()));
        assert_eq!(fds[2], (2, "pipe:[4242]".into()));
    }

    #[test]
    fn answer_split_across_reads_waits_for_done() {
        let bytes = fixture("snap-pod.bin");
        let mut d = Decoder::default();
        let (a, b) = bytes.split_at(bytes.len() - 4);
        d.push(a);
        assert!(d.next_response().is_none());
        d.push(b);
        assert_eq!(d.next_response().unwrap().procs().count(), 2);
    }

    #[test]
    fn back_to_back_answers_keep_their_seq() {
        let mut bytes = fixture("hello.bin");
        bytes.extend(fixture("snap-pod.bin"));
        let mut d = Decoder::default();
        d.push(&bytes);
        assert_eq!(d.next_response().unwrap().seq, 0);
        assert_eq!(d.next_response().unwrap().seq, 1);
        assert!(d.next_response().is_none());
    }

    #[test]
    fn separator_inside_cmdline_truncates_only_that_frame() {
        let bytes = b"\x1eF /proc/9/cmdline\nsh\0-c\0a\x1eE b\0\x1eE 0\n\x1eF /proc/9/comm\nsh\n\x1eE 0\n\x1eD 5\n";
        let r = one(bytes);
        assert_eq!(r.get("/proc/9/cmdline").unwrap().body, b"sh\0-c\0a");
        assert_eq!(r.text("/proc/9/comm").as_deref(), Some("sh\n"));
    }
}
