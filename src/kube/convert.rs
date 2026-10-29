//! API objects to the plain structs the UI side works with
//! (contracts/kube-messages.md). Only what rtop shows is kept.

use k8s_openapi::api::core::v1::{ContainerState, Pod};

use super::msg::{ContainerInfo, EphemeralInfo, PodInfo, Resources};

pub fn pod_info(pod: &Pod) -> PodInfo {
    let meta = &pod.metadata;
    let spec = pod.spec.as_ref();
    let status = pod.status.as_ref();
    let statuses = status.and_then(|s| s.container_statuses.as_deref()).unwrap_or_default();

    let containers: Vec<ContainerInfo> = spec
        .map(|s| s.containers.as_slice())
        .unwrap_or_default()
        .iter()
        .map(|c| {
            let st = statuses.iter().find(|s| s.name == c.name);
            let res = c.resources.as_ref();
            let quantities = |m: Option<
                &std::collections::BTreeMap<String, k8s_openapi::apimachinery::pkg::api::resource::Quantity>,
            >|
             -> Resources {
                m.map(|m| m.iter().map(|(k, v)| (k.clone(), v.0.clone())).collect()).unwrap_or_default()
            };
            ContainerInfo {
                name: c.name.clone(),
                id: st.and_then(|s| s.container_id.as_deref()).map(strip_runtime),
                ready: st.is_some_and(|s| s.ready),
                restarts: st.map_or(0, |s| s.restart_count),
                state: st.and_then(|s| s.state.as_ref()).map_or_else(|| "waiting".to_string(), state_text),
                last_termination: st
                    .and_then(|s| s.last_state.as_ref())
                    .filter(|s| s.terminated.is_some())
                    .map(state_text),
                requests: quantities(res.and_then(|r| r.requests.as_ref())),
                limits: quantities(res.and_then(|r| r.limits.as_ref())),
            }
        })
        .collect();

    let eph_status = status.and_then(|s| s.ephemeral_container_statuses.as_deref()).unwrap_or_default();
    let ephemeral = spec
        .and_then(|s| s.ephemeral_containers.as_deref())
        .unwrap_or_default()
        .iter()
        .map(|e| EphemeralInfo {
            name: e.name.clone(),
            target: e.target_container_name.clone(),
            running: eph_status
                .iter()
                .any(|s| s.name == e.name && s.state.as_ref().is_some_and(|st| st.running.is_some())),
        })
        .collect();

    // The pod's own reason (Evicted, ...) or the first container that is
    // stuck (CrashLoopBackOff, ImagePullBackOff, ...).
    let reason = status
        .and_then(|s| s.reason.clone())
        .or_else(|| statuses.iter().find_map(|s| s.state.as_ref()?.waiting.as_ref()?.reason.clone()));

    PodInfo {
        uid: meta.uid.clone().unwrap_or_default(),
        ns: meta.namespace.clone().unwrap_or_default(),
        name: meta.name.clone().unwrap_or_default(),
        node: spec.and_then(|s| s.node_name.clone()),
        phase: status.and_then(|s| s.phase.clone()).unwrap_or_else(|| "Unknown".into()),
        reason,
        share_process_namespace: spec.and_then(|s| s.share_process_namespace).unwrap_or(false),
        containers,
        ephemeral,
    }
}

/// `containerd://abc…` → `abc…`: the ID as it appears in cgroup paths.
fn strip_runtime(id: &str) -> String {
    id.split_once("://").map_or(id, |(_, rest)| rest).to_string()
}

fn state_text(s: &ContainerState) -> String {
    if s.running.is_some() {
        "running".into()
    } else if let Some(w) = &s.waiting {
        format!("waiting: {}", w.reason.as_deref().unwrap_or("unknown"))
    } else if let Some(t) = &s.terminated {
        format!("terminated: {} (exit {})", t.reason.as_deref().unwrap_or("unknown"), t.exit_code)
    } else {
        "unknown".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Pod {
        let path = format!("{}/tests/fixtures/kube/{name}", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn multi_container_pod() {
        let p = pod_info(&fixture("pod.json"));
        assert_eq!((p.ns.as_str(), p.name.as_str(), p.phase.as_str()), ("rtop-demo", "multi", "Running"));
        assert!(p.node.is_some() && !p.uid.is_empty());
        assert!(!p.share_process_namespace);
        let names: Vec<_> = p.containers.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["app", "sidecar"]);
        let app = &p.containers[0];
        assert!(app.ready && app.state == "running");
        let id = app.id.as_deref().unwrap();
        assert!(!id.contains("://") && id.len() == 64, "{id}");
        assert_eq!(p.reason, None);
    }

    #[test]
    fn shared_namespace_and_limits() {
        let p = pod_info(&fixture("pod-shared.json"));
        assert!(p.share_process_namespace);
        let l = pod_info(&fixture("pod-limited.json"));
        let c = &l.containers[0];
        assert_eq!(c.limits.get("cpu").map(String::as_str), Some("500m"));
        assert_eq!(c.requests.get("memory").map(String::as_str), Some("64Mi"));
    }
}
