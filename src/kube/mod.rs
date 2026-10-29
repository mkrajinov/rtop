//! Kubernetes targets: read `/proc` of a pod's containers or of a whole node
//! through the API server (specs/001-k8s-targets).
//!
//! All network I/O runs on one thread with a single-threaded tokio runtime.
//! The rest of rtop stays synchronous and talks to it only through
//! [`KubeHandle`]: requests go in, replies and events come back and are
//! picked up with a non-blocking `try_recv` on each tick, so a slow cluster
//! can never freeze the screen.
//!
//! `wire` and `attribution` are pure parsers and are compiled even without
//! the `kube` feature, so their tests run everywhere.

pub mod attribution;
pub mod wire;

#[cfg(feature = "kube")]
mod client;
#[cfg(feature = "kube")]
pub mod msg;

#[cfg(feature = "kube")]
pub use handle::KubeHandle;

#[cfg(feature = "kube")]
mod handle {
    use std::panic::AssertUnwindSafe;
    use std::sync::mpsc;
    use std::thread::JoinHandle;
    use std::time::Duration;

    use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

    use super::client;
    use super::msg::{Event, Incoming, Request};

    /// The UI side of the kube thread.
    pub struct KubeHandle {
        tx: UnboundedSender<(u64, Request)>,
        rx: mpsc::Receiver<Incoming>,
        next_id: u64,
        thread: Option<JoinHandle<()>>,
    }

    impl KubeHandle {
        pub fn start() -> Self {
            Self::spawn(None)
        }

        /// Start with a client built by `make` instead of reading the
        /// kubeconfig (tests use a mocked API server). `make` runs inside the
        /// kube thread's runtime, which a client needs for its worker task.
        pub fn with_client(make: impl FnOnce() -> kube::Client + Send + 'static, context: &str) -> Self {
            Self::spawn(Some((Box::new(make), context.to_string())))
        }

        fn spawn(preset: Option<Preset>) -> Self {
            let (tx, mut req_rx) = unbounded_channel();
            let (out, rx) = mpsc::channel();
            let thread = std::thread::Builder::new()
                .name("rtop-kube".into())
                .spawn(move || run(&mut req_rx, out, preset))
                .expect("spawn kube thread");
            KubeHandle { tx, rx, next_id: 1, thread: Some(thread) }
        }

        /// Queue a request; its reply arrives later with the returned id.
        pub fn send(&mut self, req: Request) -> u64 {
            let id = self.next_id;
            self.next_id += 1;
            // If the thread is gone the reply never comes, and callers
            // already treat a missing reply as a timeout.
            let _ = self.tx.send((id, req));
            id
        }

        /// Next reply or event, without waiting.
        pub fn try_recv(&self) -> Option<Incoming> {
            self.rx.try_recv().ok()
        }

        /// Wait for a reply; only for start-up, before the TUI runs.
        pub fn recv_timeout(&self, timeout: Duration) -> Option<Incoming> {
            self.rx.recv_timeout(timeout).ok()
        }
    }

    impl Drop for KubeHandle {
        /// Delete what rtop added to the cluster and stop the runtime.
        fn drop(&mut self) {
            let _ = self.tx.send((0, Request::Shutdown));
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    type Preset = (Box<dyn FnOnce() -> kube::Client + Send>, String);

    /// Runs the runtime; if a handler panics, report it and start over with
    /// the same request queue, so the UI keeps working.
    fn run(req_rx: &mut UnboundedReceiver<(u64, Request)>, out: mpsc::Sender<Incoming>, mut preset: Option<Preset>) {
        let mut backoff = Duration::from_secs(1);
        loop {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime");
            let client = preset.take().map(|(make, ctx)| {
                let _in_runtime = rt.enter();
                (make(), ctx)
            });
            let state = client::State::new(out.clone(), client);
            let done = std::panic::catch_unwind(AssertUnwindSafe(|| rt.block_on(state.serve(req_rx))));
            match done {
                Ok(()) => return,
                Err(panic) => {
                    let reason = panic
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| panic.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "unknown panic".into());
                    if out.send(Incoming::Event(Event::ThreadRestarted { reason })).is_err() {
                        return;
                    }
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(Duration::from_secs(30));
                }
            }
        }
    }
}
