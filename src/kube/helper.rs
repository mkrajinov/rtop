//! Node mode's helper pod (research R8, contracts/helper-pod.yaml): a
//! busybox pod pinned to the node with `hostPID`, so a reader in it sees
//! every process of the node in `/proc`.
//!
//! It is labelled as rtop's, deleted when rtop exits, and bounded by
//! `activeDeadlineSeconds` in case rtop is killed before it can delete it.
//! At start rtop deletes helpers of earlier sessions that have finished;
//! running ones may belong to someone else's session and are left alone.

use std::time::Duration;

use k8s_openapi::api::core::v1::Pod;
use kube::api::{Api, DeleteParams, ListParams, PostParams};

use super::client::map_err;
use super::msg::{ErrorCode, KubeError, Reply};

pub const MANAGED_BY: &str = "app.kubernetes.io/managed-by";
pub const ROLE: &str = "rtop.io/role";

/// `rtop-node-<node>-<4 hex>`, short enough for any node name.
pub fn name(node: &str, session: &str) -> String {
    let node: String = node.chars().take(40).collect();
    format!("rtop-node-{}-{}", node.trim_end_matches(['-', '.']), &session[..session.len().min(4)])
}

/// The helper pod as contracts/helper-pod.yaml describes it.
///
/// The process runs as root because only root (or the same UID) may open
/// other users' `/proc/<pid>/environ`, `fd/` and `maps` with SYS_PTRACE.
/// Every other capability is dropped. Root may still signal root's
/// processes, which is why rtop keeps node mode read-only.
pub fn manifest(node: &str, image: &str, deadline_secs: u32, session: &str) -> Pod {
    let pod = serde_json::json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {
            "name": name(node, session),
            "labels": {MANAGED_BY: "rtop", ROLE: "node-helper", "rtop.io/session": session},
        },
        "spec": {
            "nodeName": node,
            "hostPID": true,
            "restartPolicy": "Never",
            "activeDeadlineSeconds": deadline_secs,
            "terminationGracePeriodSeconds": 0,
            "automountServiceAccountToken": false,
            "enableServiceLinks": false,
            "tolerations": [{"operator": "Exists"}],
            "containers": [{
                "name": "reader",
                "image": image,
                "command": ["sleep", deadline_secs.to_string()],
                "securityContext": {
                    "runAsUser": 0,
                    "allowPrivilegeEscalation": false,
                    "capabilities": {"drop": ["ALL"], "add": ["SYS_PTRACE"]},
                },
                "resources": {
                    "requests": {"cpu": "10m", "memory": "16Mi"},
                    // The reader reads every process each refresh: with a few
                    // hundred, 200m made one snapshot take over a second.
                    "limits": {"cpu": "1", "memory": "64Mi"},
                },
            }],
        },
    });
    serde_json::from_value(pod).expect("the helper manifest is a valid Pod")
}

/// Create the helper and wait until it runs (image pull included).
pub async fn create(api: &Api<Pod>, pod: Pod) -> Result<Reply, KubeError> {
    let created = api.create(&PostParams::default(), &pod).await.map_err(map_err)?;
    let name = created.metadata.name.unwrap_or_default();
    for _ in 0..120 {
        let p = api.get(&name).await.map_err(map_err)?;
        let status = p.status.as_ref();
        match status.and_then(|s| s.phase.as_deref()) {
            Some("Running") => return Ok(Reply::Helper { name }),
            Some(phase @ ("Failed" | "Succeeded")) => {
                let _ = delete(api, &name).await;
                let why = status.and_then(|s| s.message.clone()).unwrap_or_default();
                return Err(KubeError::new(ErrorCode::Invalid, format!("helper pod {name} {phase}: {why}")));
            }
            _ => {}
        }
        let waiting = status
            .and_then(|s| s.container_statuses.as_ref())
            .and_then(|c| c.first()?.state.as_ref()?.waiting.as_ref()?.reason.clone());
        if let Some(why) =
            waiting.filter(|r| ["ErrImagePull", "ImagePullBackOff", "InvalidImageName"].contains(&r.as_str()))
        {
            let _ = delete(api, &name).await;
            return Err(KubeError::new(ErrorCode::Invalid, format!("helper pod {name}: {why}")));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let _ = delete(api, &name).await;
    Err(KubeError::new(ErrorCode::Timeout, format!("helper pod {name} did not start within 60 s")))
}

pub async fn delete(api: &Api<Pod>, name: &str) -> Result<Reply, KubeError> {
    match api.delete(name, &DeleteParams::default().grace_period(0)).await {
        Ok(_) => Ok(Reply::Deleted(vec![name.to_string()])),
        // Already gone (the deadline ended it, or someone deleted it).
        Err(kube::Error::Api(s)) if s.code == 404 => Ok(Reply::Deleted(vec![name.to_string()])),
        Err(e) => Err(map_err(e)),
    }
}

/// Delete rtop's helpers that have finished. Running ones may serve
/// another rtop session and stay.
pub async fn gc(api: &Api<Pod>) -> Result<Reply, KubeError> {
    let lp = ListParams::default().labels(&format!("{MANAGED_BY}=rtop,{ROLE}=node-helper"));
    let list = api.list(&lp).await.map_err(map_err)?;
    let mut deleted = Vec::new();
    for p in list.items {
        let phase = p.status.as_ref().and_then(|s| s.phase.as_deref());
        if !matches!(phase, Some("Succeeded" | "Failed")) {
            continue;
        }
        if let Some(name) = p.metadata.name
            && delete(api, &name).await.is_ok()
        {
            deleted.push(name);
        }
    }
    Ok(Reply::Deleted(deleted))
}

#[cfg(test)]
mod tests {
    use super::super::client::tests::{answer, mocked, reply};
    use super::super::msg::{ErrorCode, Reply, Request};
    use super::*;

    #[test]
    fn manifest_matches_the_contract() {
        let p = manifest("rtop-control-plane", "busybox:1.37", 3600, "a1b2c3d4");
        let m = &p.metadata;
        assert_eq!(m.name.as_deref(), Some("rtop-node-rtop-control-plane-a1b2"));
        let labels = m.labels.as_ref().unwrap();
        assert_eq!(labels[MANAGED_BY], "rtop");
        assert_eq!(labels["rtop.io/session"], "a1b2c3d4");
        let spec = p.spec.as_ref().unwrap();
        assert_eq!(spec.node_name.as_deref(), Some("rtop-control-plane"));
        assert_eq!((spec.host_pid, spec.active_deadline_seconds), (Some(true), Some(3600)));
        assert_eq!(spec.restart_policy.as_deref(), Some("Never"));
        assert_eq!(spec.tolerations.as_ref().unwrap()[0].operator.as_deref(), Some("Exists"));
        let c = &spec.containers[0];
        assert_eq!(c.command.as_ref().unwrap(), &["sleep", "3600"]);
        let caps = c.security_context.as_ref().unwrap().capabilities.as_ref().unwrap();
        assert_eq!(
            (caps.drop.as_deref(), caps.add.as_deref()),
            (Some(&["ALL".to_string()][..]), Some(&["SYS_PTRACE".to_string()][..]))
        );
        assert!(spec.volumes.is_none(), "no host paths");
        assert!(spec.host_network.is_none());
    }

    fn create_req() -> Request {
        Request::HelperCreate {
            node: "rtop-control-plane".into(),
            ns: "rtop-demo".into(),
            image: "busybox:1.37".into(),
            deadline_secs: 3600,
            session: "a1b2c3d4".into(),
        }
    }

    fn helper(phase: &str) -> serde_json::Value {
        let mut p = serde_json::to_value(manifest("rtop-control-plane", "busybox:1.37", 3600, "a1b2c3d4")).unwrap();
        p["metadata"]["namespace"] = "rtop-demo".into();
        p["status"] = serde_json::json!({"phase": phase});
        p
    }

    #[test]
    fn create_waits_until_running() {
        let (mut k, mut api) = mocked();
        let id = k.send(create_req());
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            assert_eq!(answer(&mut api, 201, helper("Pending")).await, "/api/v1/namespaces/rtop-demo/pods");
            answer(&mut api, 200, helper("Pending")).await;
            answer(&mut api, 200, helper("Running")).await;
        });
        assert_eq!(reply(&k, id), Reply::Helper { name: "rtop-node-rtop-control-plane-a1b2".into() });
    }

    #[test]
    fn pod_security_rejection_is_an_admission_error() {
        let (mut k, mut api) = mocked();
        let id = k.send(create_req());
        let msg = "pods \"rtop-node-x\" is forbidden: violates PodSecurity \"baseline:latest\": host namespaces (hostPID=true)";
        let status = serde_json::json!({"kind": "Status", "apiVersion": "v1", "status": "Failure", "code": 403,
                                        "reason": "Forbidden", "message": msg});
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(answer(&mut api, 403, status));
        let Reply::Err(e) = reply(&k, id) else { panic!() };
        assert_eq!(e.code, ErrorCode::Admission);
    }

    /// Only finished helpers go: a running one may be another session's.
    #[test]
    fn gc_deletes_only_finished_helpers() {
        let (mut k, mut api) = mocked();
        let id = k.send(Request::HelperGc { ns: "rtop-demo".into() });
        let mut running = helper("Running");
        running["metadata"]["name"] = "rtop-node-a-live".into();
        let mut failed = helper("Failed");
        failed["metadata"]["name"] = "rtop-node-a-dead".into();
        let list =
            serde_json::json!({"kind": "PodList", "apiVersion": "v1", "metadata": {}, "items": [running, failed]});
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let (req, send) = api.next_request().await.unwrap();
            let query = req.uri().query().unwrap_or_default().to_string();
            assert!(query.contains("labelSelector=app.kubernetes.io%2Fmanaged-by%3Drtop"), "{query}");
            send.send_response(
                http::Response::builder().body(kube::client::Body::from(list.to_string().into_bytes())).unwrap(),
            );
            let (req, send) = api.next_request().await.unwrap();
            assert_eq!(
                (req.method().as_str(), req.uri().path()),
                ("DELETE", "/api/v1/namespaces/rtop-demo/pods/rtop-node-a-dead")
            );
            send.send_response(
                http::Response::builder()
                    .body(kube::client::Body::from(helper("Failed").to_string().into_bytes()))
                    .unwrap(),
            );
        });
        assert_eq!(reply(&k, id), Reply::Deleted(vec!["rtop-node-a-dead".into()]));
    }
}
