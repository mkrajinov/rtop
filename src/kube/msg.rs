//! Messages between the synchronous UI side and the kube thread
//! (specs/001-k8s-targets/contracts/kube-messages.md).
//!
//! The UI side sends a [`Request`] with an id and later finds exactly one
//! [`Reply`] with the same id on the reply channel, next to unsolicited
//! [`Event`]s from watchers and exec streams. Nothing here is async.

use std::collections::BTreeMap;

pub type StreamId = u32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Build the client for this kubeconfig context (`None`: the current one).
    Hello {
        context: Option<String>,
    },
    GetPod {
        ns: String,
        name: String,
    },
    ListNamespaces,
    /// `ns` empty: every namespace.
    ListPods {
        ns: String,
    },
    ListNodes,
    GetNode {
        name: String,
    },
    WatchPod {
        ns: String,
        name: String,
    },
    WatchNodePods {
        node: String,
    },
    Probe {
        ns: String,
        pod: String,
        container: String,
    },
    EnsureDebugContainer {
        ns: String,
        pod: String,
        target: String,
        image: String,
        sleep_secs: u32,
    },
    ExecOpen {
        ns: String,
        pod: String,
        container: String,
        command: Vec<String>,
        stdin_init: Vec<u8>,
    },
    StreamWrite {
        stream: StreamId,
        data: Vec<u8>,
    },
    StreamClose {
        stream: StreamId,
    },
    HelperGc {
        ns: String,
    },
    HelperCreate {
        node: String,
        ns: String,
        image: String,
        deadline_secs: u32,
        session: String,
    },
    HelperDelete {
        ns: String,
        name: String,
    },
    Shutdown,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Reply {
    Hello {
        context: String,
        namespace: String,
        server_version: String,
    },
    Pod(PodInfo),
    Namespaces(Vec<String>),
    Pods(Vec<PodSummary>),
    Nodes(Vec<NodeSummary>),
    Node(NodeInfo),
    Probe {
        tools: bool,
        detail: String,
    },
    /// `ptrace`: false when policy refused CAP_SYS_PTRACE and the container
    /// was added without it.
    Debug {
        name: String,
        created: bool,
        ptrace: bool,
    },
    Stream(StreamId),
    Helper {
        name: String,
    },
    Deleted(Vec<String>),
    Done,
    Err(KubeError),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Pod(PodInfo),
    PodDeleted {
        ns: String,
        name: String,
    },
    NodeIndex(NodeIndex),
    StreamData {
        stream: StreamId,
        data: Vec<u8>,
    },
    StreamClosed {
        stream: StreamId,
        exit_code: Option<i32>,
        error: Option<KubeError>,
    },
    /// The kube thread panicked and was started again; every stream is gone.
    ThreadRestarted {
        reason: String,
    },
}

/// What the kube thread sends back: a reply to request `id`, or an event.
#[derive(Clone, Debug, PartialEq)]
pub enum Incoming {
    Reply(u64, Reply),
    Event(Event),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    Unreachable,
    Unauthorized,
    Forbidden,
    NotFound,
    Invalid,
    Admission,
    Timeout,
    Unsupported,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KubeError {
    pub code: ErrorCode,
    pub message: String,
    pub verb: Option<String>,
    pub resource: Option<String>,
    pub namespace: Option<String>,
}

impl KubeError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        KubeError { code, message: message.into(), verb: None, resource: None, namespace: None }
    }
}

impl std::fmt::Display for KubeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.code, &self.verb, &self.resource) {
            (ErrorCode::Forbidden, Some(verb), Some(res)) => {
                write!(f, "forbidden: {verb} {res}")?;
                if let Some(ns) = &self.namespace {
                    write!(f, " in {ns}")?;
                }
                write!(f, " (see the README for the permissions each mode needs)")
            }
            _ => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for KubeError {}

/// CPU and memory as written in the pod spec, e.g. `500m`, `256Mi`.
pub type Resources = BTreeMap<String, String>;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PodInfo {
    pub uid: String,
    pub ns: String,
    pub name: String,
    pub node: Option<String>,
    pub phase: String,
    /// Why the pod is not usable (e.g. `CrashLoopBackOff`), if it is not.
    pub reason: Option<String>,
    pub share_process_namespace: bool,
    pub containers: Vec<ContainerInfo>,
    pub ephemeral: Vec<EphemeralInfo>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContainerInfo {
    pub name: String,
    /// Runtime ID without the `containerd://` prefix.
    pub id: Option<String>,
    pub ready: bool,
    pub restarts: i32,
    /// `running`, `waiting: <reason>` or `terminated: <reason>`.
    pub state: String,
    pub last_termination: Option<String>,
    pub requests: Resources,
    pub limits: Resources,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EphemeralInfo {
    pub name: String,
    pub target: Option<String>,
    pub running: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PodSummary {
    pub ns: String,
    pub name: String,
    pub phase: String,
    pub reason: Option<String>,
    pub node: Option<String>,
    pub containers: usize,
    pub ready: usize,
    pub restarts: i32,
    /// `metadata.creationTimestamp` in Unix seconds.
    pub created: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NodeSummary {
    pub name: String,
    pub ready: bool,
    pub roles: Vec<String>,
    pub pods: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NodeInfo {
    pub name: String,
    pub ready: bool,
    pub allocatable: Resources,
    pub pods: usize,
}

/// Which pod and container every container ID on a node belongs to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NodeIndex {
    /// pod UID → (namespace, name)
    pub pods: BTreeMap<String, (String, String)>,
    /// container ID → (pod UID, container name)
    pub containers: BTreeMap<String, (String, String)>,
}
