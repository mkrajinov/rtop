//! The async side: one [`State`] per runtime, answering [`Request`]s with
//! kube-rs calls. Handlers for debug containers and the node helper are
//! added story by story; until then they answer `Unsupported`.
//!
//! A request handler only starts long-running work: watchers and exec
//! sessions run as their own tasks and report through [`Event`]s, so one
//! slow container never holds up the replies to the others.

use std::collections::HashMap;
use std::sync::mpsc;

use futures::StreamExt;
use k8s_openapi::api::core::v1::{Namespace, Node, Pod};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Status;
use kube::Client;
use kube::api::{Api, AttachParams, ListParams};
use kube::config::{Config, KubeConfigOptions, Kubeconfig};
use kube::runtime::{WatchStreamExt, watcher};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use super::convert;
use super::msg::{ErrorCode, Event, Incoming, KubeError, Reply, Request, StreamId};

/// What the UI side can send to a running exec session: bytes for its
/// stdin, or `None` to end it.
type StreamInput = UnboundedSender<Option<Vec<u8>>>;

pub struct State {
    out: mpsc::Sender<Incoming>,
    client: Option<Client>,
    context: String,
    namespace: String,
    streams: HashMap<StreamId, StreamInput>,
    next_stream: StreamId,
}

impl State {
    pub fn new(out: mpsc::Sender<Incoming>, preset: Option<(Client, String)>) -> Self {
        let (client, context) = match preset {
            Some((c, ctx)) => (Some(c), ctx),
            None => (None, String::new()),
        };
        let namespace = client.as_ref().map_or_else(String::new, |c| c.default_namespace().to_string());
        State { out, client, context, namespace, streams: HashMap::new(), next_stream: 1 }
    }

    /// Answer requests until `Shutdown` or until the UI side is gone.
    pub async fn serve(mut self, rx: &mut UnboundedReceiver<(u64, Request)>) {
        while let Some((id, req)) = rx.recv().await {
            // Waiting for a debug container can take a minute (image pull):
            // answer from a task so stream writes keep flowing meanwhile.
            if let Request::EnsureDebugContainer { ns, pod, target, image, sleep_secs } = req {
                let (api, out) = (self.pods(&ns), self.out.clone());
                tokio::spawn(async move {
                    let reply = match api {
                        Ok(api) => super::ephemeral::ensure(&api, &pod, &target, &image, sleep_secs).await,
                        Err(e) => Err(e),
                    };
                    let _ = out.send(Incoming::Reply(id, reply.unwrap_or_else(Reply::Err)));
                });
                continue;
            }
            if let Request::HelperCreate { node, ns, image, deadline_secs, session } = req {
                let (api, out) = (self.pods(&ns), self.out.clone());
                let pod = super::helper::manifest(&node, &image, deadline_secs, &session);
                tokio::spawn(async move {
                    let reply = match api {
                        Ok(api) => super::helper::create(&api, pod).await,
                        Err(e) => Err(e),
                    };
                    let _ = out.send(Incoming::Reply(id, reply.unwrap_or_else(Reply::Err)));
                });
                continue;
            }
            if let Request::ExecOpen { ns, pod, container, command, stdin_init } = req {
                self.exec_open(id, &ns, &pod, &container, command, stdin_init);
                continue;
            }
            let stop = req == Request::Shutdown;
            let reply = self.handle(req).await;
            if self.out.send(Incoming::Reply(id, reply)).is_err() || stop {
                return;
            }
        }
    }

    async fn handle(&mut self, req: Request) -> Reply {
        let result = match req {
            Request::Hello { context } => self.hello(context).await,
            Request::GetPod { ns, name } => self.get_pod(&ns, &name).await,
            Request::ListNamespaces => self.list_namespaces().await,
            Request::ListPods { ns } => self.list_pods(&ns).await,
            Request::WatchPod { ns, name } => self.watch_pod(ns, name),
            Request::GetNode { name } => self.get_node(&name).await,
            Request::WatchNodePods { node } => self.watch_node_pods(node),
            Request::HelperDelete { ns, name } => match self.pods(&ns) {
                Ok(api) => super::helper::delete(&api, &name).await,
                Err(e) => Err(e),
            },
            Request::HelperGc { ns } => match self.pods(&ns) {
                Ok(api) => super::helper::gc(&api).await,
                Err(e) => Err(e),
            },
            Request::StreamWrite { stream, data } => self.stream_input(stream, Some(data)),
            Request::StreamClose { stream } => {
                let r = self.stream_input(stream, None);
                self.streams.remove(&stream);
                r
            }
            Request::Shutdown => Ok(Reply::Done),
            other => Err(KubeError::new(ErrorCode::Unsupported, format!("not implemented yet: {other:?}"))),
        };
        result.unwrap_or_else(Reply::Err)
    }

    async fn hello(&mut self, context: Option<String>) -> Result<Reply, KubeError> {
        if self.client.is_none() {
            let (client, ctx, ns) = connect(context).await?;
            self.client = Some(client);
            self.context = ctx;
            self.namespace = ns;
        }
        let info = self.client()?.apiserver_version().await.map_err(map_err)?;
        Ok(Reply::Hello {
            context: self.context.clone(),
            namespace: self.namespace.clone(),
            server_version: info.git_version,
        })
    }

    fn client(&self) -> Result<&Client, KubeError> {
        self.client.as_ref().ok_or_else(|| KubeError::new(ErrorCode::Internal, "no client: send Hello first"))
    }

    fn pods(&self, ns: &str) -> Result<Api<Pod>, KubeError> {
        Ok(Api::namespaced(self.client()?.clone(), ns))
    }

    async fn get_pod(&self, ns: &str, name: &str) -> Result<Reply, KubeError> {
        let pod = self.pods(ns)?.get(name).await.map_err(map_err)?;
        Ok(Reply::Pod(convert::pod_info(&pod)))
    }

    async fn list_namespaces(&self) -> Result<Reply, KubeError> {
        let api: Api<Namespace> = Api::all(self.client()?.clone());
        let list = api.list(&ListParams::default().match_any()).await.map_err(map_err)?;
        let mut names: Vec<String> = list.items.into_iter().filter_map(|n| n.metadata.name).collect();
        names.sort();
        Ok(Reply::Namespaces(names))
    }

    /// Picker lists are served from the API server's watch cache
    /// (`resourceVersion=0`), so polling them never reaches etcd. The data
    /// may be a moment old, which a picker can live with.
    async fn list_pods(&self, ns: &str) -> Result<Reply, KubeError> {
        let api: Api<Pod> = if ns.is_empty() { Api::all(self.client()?.clone()) } else { self.pods(ns)? };
        let list = api.list(&ListParams::default().match_any()).await.map_err(map_err)?;
        let mut pods: Vec<_> = list.items.iter().map(convert::pod_summary).collect();
        pods.sort_by(|a, b| (&a.ns, &a.name).cmp(&(&b.ns, &b.name)));
        Ok(Reply::Pods(pods))
    }

    async fn get_node(&self, name: &str) -> Result<Reply, KubeError> {
        let api: Api<Node> = Api::all(self.client()?.clone());
        let node = api.get(name).await.map_err(map_err)?;
        Ok(Reply::Node(convert::node_info(&node)))
    }

    /// Keep an index of the pods on a node, for attributing its processes
    /// by cgroup. Changes come in bursts (a rollout), so the index is sent
    /// at most once a second.
    fn watch_node_pods(&self, node: String) -> Result<Reply, KubeError> {
        let api: Api<Pod> = Api::all(self.client()?.clone());
        let out = self.out.clone();
        let cfg = watcher::Config::default().fields(&format!("spec.nodeName={node}"));
        tokio::spawn(async move {
            let mut events = watcher(api, cfg).default_backoff().boxed();
            let mut pods: HashMap<String, Pod> = HashMap::new();
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
            let mut dirty = false;
            loop {
                tokio::select! {
                    ev = events.next() => match ev {
                        Some(Ok(watcher::Event::Apply(p) | watcher::Event::InitApply(p))) => {
                            pods.insert(p.metadata.uid.clone().unwrap_or_default(), p);
                            dirty = true;
                        }
                        Some(Ok(watcher::Event::Delete(p))) => {
                            pods.remove(p.metadata.uid.as_deref().unwrap_or_default());
                            dirty = true;
                        }
                        Some(Ok(watcher::Event::Init)) => pods.clear(),
                        Some(_) => {}
                        None => return,
                    },
                    _ = tick.tick(), if dirty => {
                        dirty = false;
                        let ev = Event::NodeIndex(convert::node_index(pods.values()));
                        if out.send(Incoming::Event(ev)).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        Ok(Reply::Done)
    }

    /// Follow one pod: restarts, new ephemeral containers, deletion. The
    /// watcher re-lists and re-watches on its own after errors.
    fn watch_pod(&self, ns: String, name: String) -> Result<Reply, KubeError> {
        let api = self.pods(&ns)?;
        let out = self.out.clone();
        let cfg = watcher::Config::default().fields(&format!("metadata.name={name}"));
        tokio::spawn(async move {
            let mut events = watcher(api, cfg).default_backoff().boxed();
            while let Some(ev) = events.next().await {
                let ev = match ev {
                    Ok(watcher::Event::Apply(p) | watcher::Event::InitApply(p)) => Event::Pod(convert::pod_info(&p)),
                    Ok(watcher::Event::Delete(_)) => Event::PodDeleted { ns: ns.clone(), name: name.clone() },
                    // Init/InitDone, and errors the backoff retries.
                    _ => continue,
                };
                if out.send(Incoming::Event(ev)).is_err() {
                    return;
                }
            }
        });
        Ok(Reply::Done)
    }

    /// Start `command` in a container with stdin and without a TTY (a TTY
    /// would turn `\n` into `\r\n` and mix stderr into stdout).
    ///
    /// The stream id and its input queue exist at once; connecting happens
    /// in a task that also sends the reply. Against an unreachable API
    /// server the connect can hang for a long time, and it must hold up
    /// neither the other sessions nor `Shutdown`. Writes sent meanwhile wait
    /// in the queue.
    fn exec_open(&mut self, req: u64, ns: &str, pod: &str, container: &str, command: Vec<String>, stdin_init: Vec<u8>) {
        let id = self.next_stream;
        self.next_stream += 1;
        let (tx, rx) = unbounded_channel();
        self.streams.insert(id, tx);
        let out = self.out.clone();
        let api = self.pods(ns);
        let (pod, ap) = (
            pod.to_string(),
            AttachParams::default().container(container).stdin(true).stdout(true).stderr(true).tty(false),
        );
        tokio::spawn(async move {
            let started = async {
                let mut proc = api?.exec(&pod, command, &ap).await.map_err(map_err)?;
                let (Some(mut stdin), Some(stdout), Some(stderr), Some(status)) =
                    (proc.stdin(), proc.stdout(), proc.stderr(), proc.take_status())
                else {
                    return Err(KubeError::new(ErrorCode::Internal, "exec session without stdio"));
                };
                stdin
                    .write_all(&stdin_init)
                    .await
                    .map_err(|e| KubeError::new(ErrorCode::Unreachable, e.to_string()))?;
                Ok((proc, stdin, stdout, stderr, status))
            };
            let (proc, stdin, stdout, stderr, status) = match started.await {
                Ok(parts) => parts,
                Err(e) => {
                    let _ = out.send(Incoming::Reply(req, Reply::Err(e)));
                    return;
                }
            };
            if out.send(Incoming::Reply(req, Reply::Stream(id))).is_err() {
                return;
            }
            let (ended, stderr) = pump(id, stdin, stdout, stderr, rx, &out).await;
            let (exit_code, error) = match ended {
                // stdout ended: the command exited, the status says how.
                Ended::Eof => exit_status(status.await),
                Ended::Closed => (None, None),
                Ended::Failed(e) => (None, Some(KubeError::new(ErrorCode::Unreachable, e))),
            };
            let error = error.map(|mut e| {
                if !stderr.is_empty() {
                    e.message = format!("{} ({})", e.message, stderr.trim());
                }
                e
            });
            proc.abort();
            let _ = out.send(Incoming::Event(Event::StreamClosed { stream: id, exit_code, error }));
        });
    }

    fn stream_input(&self, stream: StreamId, data: Option<Vec<u8>>) -> Result<Reply, KubeError> {
        let closed = || KubeError::new(ErrorCode::NotFound, format!("stream {stream} is closed"));
        let tx = self.streams.get(&stream).ok_or_else(closed)?;
        tx.send(data).map_err(|_| closed())?;
        Ok(Reply::Done)
    }
}

#[derive(Debug, PartialEq)]
enum Ended {
    /// The command closed its stdout (it exited).
    Eof,
    /// The UI side asked to stop.
    Closed,
    Failed(String),
}

/// Copy one session until it ends: `input` to stdin, stdout to
/// `StreamData` events. stderr is drained so the multiplexer never stalls;
/// its last few hundred bytes are returned for the error message.
async fn pump(
    id: StreamId,
    mut stdin: impl AsyncWrite + Unpin,
    mut stdout: impl AsyncRead + Unpin,
    mut stderr: impl AsyncRead + Unpin,
    mut input: UnboundedReceiver<Option<Vec<u8>>>,
    out: &mpsc::Sender<Incoming>,
) -> (Ended, String) {
    let mut buf = vec![0u8; 64 * 1024];
    let mut ebuf = [0u8; 4096];
    let mut tail = Vec::new();
    let mut stderr_open = true;
    let ended = loop {
        tokio::select! {
            msg = input.recv() => match msg {
                Some(Some(data)) => {
                    if let Err(e) = stdin.write_all(&data).await {
                        break Ended::Failed(e.to_string());
                    }
                }
                Some(None) | None => break Ended::Closed,
            },
            n = stdout.read(&mut buf) => match n {
                Ok(0) => break Ended::Eof,
                Ok(n) => {
                    let ev = Event::StreamData { stream: id, data: buf[..n].to_vec() };
                    if out.send(Incoming::Event(ev)).is_err() {
                        break Ended::Closed;
                    }
                }
                Err(e) => break Ended::Failed(e.to_string()),
            },
            n = stderr.read(&mut ebuf), if stderr_open => match n {
                Ok(0) | Err(_) => stderr_open = false,
                Ok(n) => {
                    tail.extend_from_slice(&ebuf[..n]);
                    let cut = tail.len().saturating_sub(512);
                    tail.drain(..cut);
                }
            },
        }
    };
    // stdout can close before the last stderr bytes are read; they usually
    // say why the command ended, so give them a moment.
    if ended == Ended::Eof && stderr_open {
        let rest = async {
            while let Ok(n @ 1..) = stderr.read(&mut ebuf).await {
                tail.extend_from_slice(&ebuf[..n]);
            }
        };
        let _ = tokio::time::timeout(std::time::Duration::from_millis(200), rest).await;
        let cut = tail.len().saturating_sub(512);
        tail.drain(..cut);
    }
    (ended, String::from_utf8_lossy(&tail).into_owned())
}

/// The exit code from the status the API server sends when an exec ends:
/// `Success`, or `Failure` with an `ExitCode` cause.
fn exit_status(status: Option<Status>) -> (Option<i32>, Option<KubeError>) {
    let Some(s) = status else { return (None, None) };
    if s.status.as_deref() == Some("Success") {
        return (Some(0), None);
    }
    let code = s
        .details
        .as_ref()
        .and_then(|d| d.causes.as_ref())
        .and_then(|c| c.iter().find(|c| c.reason.as_deref() == Some("ExitCode")))
        .and_then(|c| c.message.as_deref()?.parse().ok());
    match code {
        Some(c) => (Some(c), None),
        None => (None, Some(KubeError::new(ErrorCode::Internal, s.message.unwrap_or_default()))),
    }
}

/// Read the kubeconfig the way kubectl does ($KUBECONFIG or ~/.kube/config,
/// exec credential plugins included) and build a client for one context.
async fn connect(context: Option<String>) -> Result<(Client, String, String), KubeError> {
    let invalid = |e: &dyn std::fmt::Display| KubeError::new(ErrorCode::Invalid, format!("kubeconfig: {e}"));
    let kubeconfig = Kubeconfig::read().map_err(|e| invalid(&e))?;
    let context = context
        .or_else(|| kubeconfig.current_context.clone())
        .ok_or_else(|| KubeError::new(ErrorCode::Invalid, "kubeconfig has no current context; pass --context"))?;
    if !kubeconfig.contexts.iter().any(|c| c.name == context) {
        return Err(KubeError::new(ErrorCode::Invalid, format!("context {context}: not in the kubeconfig")));
    }
    let options = KubeConfigOptions { context: Some(context.clone()), ..Default::default() };
    let config = Config::from_custom_kubeconfig(kubeconfig, &options)
        .await
        .map_err(|e| KubeError::new(ErrorCode::Unauthorized, format!("credentials for {context}: {e}")))?;
    let namespace = config.default_namespace.clone();
    let client = Client::try_from(config).map_err(map_err)?;
    Ok((client, context, namespace))
}

/// Turn a kube-rs error into the codes the UI explains (FR-012).
pub fn map_err(e: kube::Error) -> KubeError {
    match e {
        kube::Error::Api(status) => {
            let code = match status.code {
                401 => ErrorCode::Unauthorized,
                403 if is_admission(&status.message) => ErrorCode::Admission,
                403 => ErrorCode::Forbidden,
                404 => ErrorCode::NotFound,
                400 | 422 if is_admission(&status.message) => ErrorCode::Admission,
                422 => ErrorCode::Invalid,
                408 | 504 => ErrorCode::Timeout,
                _ => ErrorCode::Internal,
            };
            let mut err = KubeError::new(code, status.message.clone());
            if code == ErrorCode::Forbidden {
                // "pods \"x\" is forbidden: User \"u\" cannot create resource
                // \"pods/exec\" in API group \"\" in the namespace \"ns\""
                err.verb = between(&status.message, "cannot ", " resource").map(str::to_string);
                err.resource = between(&status.message, "resource \"", "\"").map(str::to_string);
                err.namespace = between(&status.message, "in the namespace \"", "\"").map(str::to_string);
            }
            err
        }
        kube::Error::Auth(e) => KubeError::new(ErrorCode::Unauthorized, format!("credentials: {e}")),
        e @ (kube::Error::HyperError(_) | kube::Error::Service(_) | kube::Error::RustlsTls(_)) => {
            KubeError::new(ErrorCode::Unreachable, format!("API server unreachable: {e}"))
        }
        e => KubeError::new(ErrorCode::Internal, e.to_string()),
    }
}

fn is_admission(message: &str) -> bool {
    message.contains("violates PodSecurity") || message.contains("admission webhook")
}

fn between<'a>(s: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let rest = &s[s.find(start)? + start.len()..];
    Some(&rest[..rest.find(end)?])
}

#[cfg(test)]
pub(crate) mod tests {
    use std::time::Duration;

    use http::{Request as HttpRequest, Response};
    use kube::client::Body;
    use tower_test::mock;

    use super::super::KubeHandle;
    use super::super::msg::{ErrorCode, Event, Incoming, Reply, Request};

    pub type Handle = mock::Handle<HttpRequest<Body>, Response<Body>>;

    /// A kube thread whose client talks to a scripted API server.
    pub fn mocked() -> (KubeHandle, Handle) {
        let (service, handle) = mock::pair::<HttpRequest<Body>, Response<Body>>();
        (KubeHandle::with_client(move || kube::Client::new(service, "rtop-demo"), "kind-rtop"), handle)
    }

    /// Answer the next API call with `status` and a JSON body; returns the
    /// request path so tests can check what was asked.
    pub async fn answer(handle: &mut Handle, status: u16, body: serde_json::Value) -> String {
        let (req, send) = handle.next_request().await.expect("an API request");
        let path = req.uri().path().to_string();
        let resp = Response::builder().status(status).body(Body::from(body.to_string().into_bytes())).unwrap();
        send.send_response(resp);
        path
    }

    pub fn reply(k: &KubeHandle, id: u64) -> Reply {
        match k.recv_timeout(Duration::from_secs(5)) {
            Some(Incoming::Reply(got, r)) if got == id => r,
            other => panic!("expected reply {id}, got {other:?}"),
        }
    }

    fn status(code: u16, reason: &str, message: &str) -> serde_json::Value {
        serde_json::json!({"kind": "Status", "apiVersion": "v1", "status": "Failure",
                           "code": code, "reason": reason, "message": message})
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
    }

    #[test]
    fn hello_reports_context_namespace_and_version() {
        let (mut k, mut api) = mocked();
        let id = k.send(Request::Hello { context: None });
        let path = rt().block_on(answer(
            &mut api,
            200,
            serde_json::json!({
                "major": "1", "minor": "33", "gitVersion": "v1.33.1", "gitCommit": "", "gitTreeState": "",
                "buildDate": "", "goVersion": "", "compiler": "", "platform": "linux/arm64"
            }),
        ));
        assert_eq!(path, "/version");
        assert_eq!(
            reply(&k, id),
            Reply::Hello {
                context: "kind-rtop".into(),
                namespace: "rtop-demo".into(),
                server_version: "v1.33.1".into()
            }
        );
    }

    #[test]
    fn api_errors_map_to_codes() {
        let forbidden = "pods \"multi\" is forbidden: User \"dev\" cannot create resource \"pods/exec\" in API group \"\" in the namespace \"rtop-demo\"";
        let cases = [
            (401, "Unauthorized", "Unauthorized", ErrorCode::Unauthorized),
            (403, "Forbidden", forbidden, ErrorCode::Forbidden),
            (404, "NotFound", "not found", ErrorCode::NotFound),
            (
                403,
                "Forbidden",
                "pods \"x\" is forbidden: violates PodSecurity \"baseline:latest\": host namespaces",
                ErrorCode::Admission,
            ),
        ];
        for (code, reason, message, want) in cases {
            let (mut k, mut api) = mocked();
            let id = k.send(Request::Hello { context: None });
            rt().block_on(answer(&mut api, code, status(code, reason, message)));
            let Reply::Err(e) = reply(&k, id) else { panic!("{code} should fail") };
            assert_eq!(e.code, want, "{code} {message}");
            if want == ErrorCode::Forbidden {
                assert_eq!(
                    e.to_string(),
                    "forbidden: create pods/exec in rtop-demo (see the README for the permissions each mode needs)"
                );
            }
        }
    }

    #[test]
    fn lists_namespaces_and_pods() {
        let (mut k, mut api) = mocked();
        let ns = k.send(Request::ListNamespaces);
        let path = rt().block_on(answer(
            &mut api,
            200,
            serde_json::json!({"kind": "NamespaceList", "apiVersion": "v1", "metadata": {},
                               "items": [{"metadata": {"name": "rtop-demo"}}, {"metadata": {"name": "default"}}]}),
        ));
        assert_eq!(path, "/api/v1/namespaces");
        assert_eq!(reply(&k, ns), Reply::Namespaces(vec!["default".into(), "rtop-demo".into()]));

        let pods = k.send(Request::ListPods { ns: "rtop-demo".into() });
        let list = serde_json::json!({"kind": "PodList", "apiVersion": "v1", "metadata": {}, "items": [pod_json()]});
        let path = rt().block_on(answer(&mut api, 200, list.clone()));
        assert_eq!(path, "/api/v1/namespaces/rtop-demo/pods");
        let Reply::Pods(p) = reply(&k, pods) else { panic!() };
        assert_eq!((p.len(), p[0].name.as_str(), p[0].ready), (1, "multi", 2));

        let all = k.send(Request::ListPods { ns: String::new() });
        assert_eq!(rt().block_on(answer(&mut api, 200, list)), "/api/v1/pods");
        assert!(matches!(reply(&k, all), Reply::Pods(_)));
    }

    /// Namespace-scoped users cannot list namespaces; the picker falls back
    /// to the context's namespace, so this must arrive as Forbidden.
    #[test]
    fn listing_namespaces_can_be_forbidden() {
        let (mut k, mut api) = mocked();
        let id = k.send(Request::ListNamespaces);
        let msg = "namespaces is forbidden: User \"dev\" cannot list resource \"namespaces\" in API group \"\" at the cluster scope";
        rt().block_on(answer(&mut api, 403, status(403, "Forbidden", msg)));
        let Reply::Err(e) = reply(&k, id) else { panic!() };
        assert_eq!(e.code, ErrorCode::Forbidden);
    }

    fn pod_json() -> serde_json::Value {
        let path = format!("{}/tests/fixtures/kube/pod.json", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn get_pod_converts_the_object() {
        let (mut k, mut api) = mocked();
        let id = k.send(Request::GetPod { ns: "rtop-demo".into(), name: "multi".into() });
        let path = rt().block_on(answer(&mut api, 200, pod_json()));
        assert_eq!(path, "/api/v1/namespaces/rtop-demo/pods/multi");
        let Reply::Pod(p) = reply(&k, id) else { panic!() };
        assert_eq!(p.containers.len(), 2);
    }

    #[test]
    fn get_pod_not_found() {
        let (mut k, mut api) = mocked();
        let id = k.send(Request::GetPod { ns: "rtop-demo".into(), name: "nope".into() });
        rt().block_on(answer(&mut api, 404, status(404, "NotFound", "pods \"nope\" not found")));
        let Reply::Err(e) = reply(&k, id) else { panic!() };
        assert_eq!(e.code, ErrorCode::NotFound);
    }

    #[test]
    fn watch_pod_reports_the_listed_pod() {
        let (mut k, mut api) = mocked();
        let id = k.send(Request::WatchPod { ns: "rtop-demo".into(), name: "multi".into() });
        assert_eq!(reply(&k, id), Reply::Done);
        let list = serde_json::json!({"apiVersion": "v1", "kind": "PodList",
                                      "metadata": {"resourceVersion": "1"}, "items": [pod_json()]});
        let rt = rt();
        let (req, send) = rt.block_on(api.next_request()).expect("a list call");
        assert!(req.uri().query().unwrap_or("").contains("fieldSelector=metadata.name%3Dmulti"), "{}", req.uri());
        send.send_response(Response::builder().body(Body::from(list.to_string().into_bytes())).unwrap());
        match k.recv_timeout(Duration::from_secs(5)) {
            Some(Incoming::Event(Event::Pod(p))) => assert_eq!(p.name, "multi"),
            other => panic!("expected a pod event, got {other:?}"),
        }
    }

    /// Exec needs a WebSocket upgrade the mock cannot fake, so the session
    /// loop is tested on in-memory pipes; the real exec is checked on kind.
    #[test]
    fn pump_moves_bytes_both_ways() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        rt().block_on(async {
            let (stdin, mut remote_in) = tokio::io::duplex(1024);
            let (mut remote_out, stdout) = tokio::io::duplex(1024);
            let (mut remote_err, stderr) = tokio::io::duplex(1024);
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            let (out, events) = std::sync::mpsc::channel();
            let pump = tokio::spawn(async move { super::pump(7, stdin, stdout, stderr, rx, &out).await });

            tx.send(Some(b"snap 1\n".to_vec())).unwrap();
            let mut got = [0u8; 7];
            remote_in.read_exact(&mut got).await.unwrap();
            assert_eq!(&got, b"snap 1\n");

            remote_err.write_all(b"sh: oops\n").await.unwrap();
            remote_out.write_all(b"\x1eD 1\n").await.unwrap();
            drop(remote_out);
            let (ended, stderr) = pump.await.unwrap();
            assert_eq!(ended, super::Ended::Eof);
            assert_eq!(stderr, "sh: oops\n");
            let data: Vec<u8> = events
                .try_iter()
                .flat_map(|e| match e {
                    Incoming::Event(Event::StreamData { stream: 7, data }) => data,
                    other => panic!("{other:?}"),
                })
                .collect();
            assert_eq!(data, b"\x1eD 1\n");
        });
    }

    #[test]
    fn exit_status_reads_the_exit_code_cause() {
        let fail: k8s_openapi::apimachinery::pkg::apis::meta::v1::Status = serde_json::from_value(serde_json::json!({
            "status": "Failure", "message": "command terminated with non-zero exit code",
            "reason": "NonZeroExitCode", "details": {"causes": [{"reason": "ExitCode", "message": "127"}]}
        }))
        .unwrap();
        assert_eq!(super::exit_status(Some(fail)), (Some(127), None));
        let ok = serde_json::from_value(serde_json::json!({"status": "Success"})).unwrap();
        assert_eq!(super::exit_status(Some(ok)), (Some(0), None));
    }

    #[test]
    fn unknown_requests_are_unsupported_not_fatal() {
        let (mut k, _api) = mocked();
        let id = k.send(Request::ListNodes);
        let Reply::Err(e) = reply(&k, id) else { panic!() };
        assert_eq!(e.code, ErrorCode::Unsupported);
        // The thread still answers afterwards.
        let id = k.send(Request::Shutdown);
        assert_eq!(reply(&k, id), Reply::Done);
    }
}
