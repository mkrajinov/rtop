//! The fallback for images without a shell (research R3, FR-003): an
//! ephemeral debug container that shares the target container's PID
//! namespace (`targetContainerName`), so a reader in it sees the target's
//! processes in `/proc`.
//!
//! Ephemeral containers cannot be removed or restarted; this one runs
//! `sleep` for an hour and then stays in the pod spec as terminated until
//! the pod is deleted. rtop reuses a running one instead of adding another.

use std::time::Duration;

use k8s_openapi::api::core::v1::Pod;
use kube::api::{Api, Patch, PatchParams};

use super::client::map_err;
use super::msg::{ErrorCode, KubeError, Reply};

/// Every debug container rtop adds is named `rtop-<target>-<suffix>`; an
/// ephemeral container has no labels, so the name is how rtop finds it.
pub fn prefix(target: &str) -> String {
    format!("rtop-{}-", &target[..target.len().min(50)])
}

/// A running debug container of rtop's for `target`, if there is one.
fn running(pod: &Pod, target: &str) -> Option<String> {
    let prefix = prefix(target);
    let spec = pod.spec.as_ref()?.ephemeral_containers.as_ref()?;
    let status = pod.status.as_ref()?.ephemeral_container_statuses.as_ref()?;
    spec.iter()
        .filter(|e| e.name.starts_with(&prefix) && e.target_container_name.as_deref() == Some(target))
        .find(|e| status.iter().any(|s| s.name == e.name && s.state.as_ref().is_some_and(|st| st.running.is_some())))
        .map(|e| e.name.clone())
}

/// Why the debug container cannot start (e.g. `ImagePullBackOff`).
fn stuck(pod: &Pod, name: &str) -> Option<String> {
    let s = pod.status.as_ref()?.ephemeral_container_statuses.as_ref()?.iter().find(|s| s.name == name)?;
    let state = s.state.as_ref()?;
    if let Some(t) = &state.terminated {
        return Some(format!("terminated: {}", t.reason.as_deref().unwrap_or("unknown")));
    }
    let reason = state.waiting.as_ref()?.reason.clone()?;
    ["ErrImagePull", "ImagePullBackOff", "InvalidImageName", "CreateContainerError"]
        .contains(&reason.as_str())
        .then_some(reason)
}

fn suffix() -> String {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().subsec_nanos();
    format!("{:05x}", n & 0xfffff)
}

fn spec(name: &str, target: &str, image: &str, sleep_secs: u32, ptrace: bool) -> serde_json::Value {
    // SYS_PTRACE lets the reader open fd/, environ and maps of processes
    // that run as another user. PodSecurity "baseline" forbids it.
    let security =
        if ptrace { serde_json::json!({"capabilities": {"add": ["SYS_PTRACE"]}}) } else { serde_json::json!({}) };
    serde_json::json!({"spec": {"ephemeralContainers": [{
        "name": name,
        "image": image,
        "command": ["sleep", sleep_secs.to_string()],
        "targetContainerName": target,
        "securityContext": security,
    }]}})
}

pub async fn ensure(api: &Api<Pod>, pod: &str, target: &str, image: &str, sleep_secs: u32) -> Result<Reply, KubeError> {
    let current = api.get(pod).await.map_err(map_err)?;
    if let Some(name) = running(&current, target) {
        return Ok(Reply::Debug { name, created: false, ptrace: true });
    }
    let name = format!("{}{}", prefix(target), suffix());
    let pp = PatchParams::default();
    let mut ptrace = true;
    let added =
        api.patch_ephemeral_containers(pod, &pp, &Patch::Strategic(spec(&name, target, image, sleep_secs, true))).await;
    match added.map_err(map_err) {
        Ok(_) => {}
        // Try once more without the capability the policy refused.
        Err(e) if e.code == ErrorCode::Admission => {
            ptrace = false;
            let plain = spec(&name, target, image, sleep_secs, false);
            api.patch_ephemeral_containers(pod, &pp, &Patch::Strategic(plain)).await.map_err(map_err)?;
        }
        Err(e) => return Err(e),
    }
    // Pulling the image can take a while on a fresh node.
    for _ in 0..120 {
        let p = api.get(pod).await.map_err(map_err)?;
        if running(&p, target).as_deref() == Some(name.as_str()) {
            return Ok(Reply::Debug { name, created: true, ptrace });
        }
        if let Some(why) = stuck(&p, &name) {
            return Err(KubeError::new(ErrorCode::Invalid, format!("debug container {name} ({image}): {why}")));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err(KubeError::new(ErrorCode::Timeout, format!("debug container {name} did not start within 60 s")))
}

#[cfg(test)]
mod tests {
    use super::super::client::tests::{answer, mocked, reply};
    use super::super::msg::{Reply, Request};

    fn pod(ephemeral: serde_json::Value, statuses: serde_json::Value) -> serde_json::Value {
        let path = format!("{}/tests/fixtures/kube/pod.json", env!("CARGO_MANIFEST_DIR"));
        let mut p: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        p["spec"]["ephemeralContainers"] = ephemeral;
        p["status"]["ephemeralContainerStatuses"] = statuses;
        p
    }

    fn running(name: &str) -> (serde_json::Value, serde_json::Value) {
        (
            serde_json::json!([{"name": name, "image": "busybox:1.37", "targetContainerName": "app"}]),
            serde_json::json!([{"name": name, "image": "busybox:1.37", "imageID": "", "ready": false,
                                "restartCount": 0, "state": {"running": {}}}]),
        )
    }

    fn ensure() -> Request {
        Request::EnsureDebugContainer {
            ns: "rtop-demo".into(),
            pod: "multi".into(),
            target: "app".into(),
            image: "busybox:1.37".into(),
            sleep_secs: 3600,
        }
    }

    #[test]
    fn reuses_a_running_debug_container() {
        let (mut k, mut api) = mocked();
        let id = k.send(ensure());
        let (spec, status) = running("rtop-app-abcde");
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(answer(&mut api, 200, pod(spec, status)));
        assert_eq!(reply(&k, id), Reply::Debug { name: "rtop-app-abcde".into(), created: false, ptrace: true });
    }

    #[test]
    fn appends_one_and_waits_until_it_runs() {
        let (mut k, mut api) = mocked();
        let id = k.send(ensure());
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            answer(&mut api, 200, pod(serde_json::json!([]), serde_json::json!([]))).await;
            let (req, send) = api.next_request().await.unwrap();
            assert_eq!(req.method(), "PATCH");
            assert_eq!(req.uri().path(), "/api/v1/namespaces/rtop-demo/pods/multi/ephemeralcontainers");
            let body = http_body_util::BodyExt::collect(req.into_body()).await.unwrap().to_bytes();
            let patch: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let c = &patch["spec"]["ephemeralContainers"][0];
            assert_eq!(c["targetContainerName"], "app");
            assert_eq!(c["securityContext"]["capabilities"]["add"][0], "SYS_PTRACE");
            let name = c["name"].as_str().unwrap().to_string();
            assert!(name.starts_with("rtop-app-"));
            let (spec, status) = running(&name);
            let body = pod(spec, status).to_string().into_bytes();
            send.send_response(http::Response::builder().body(kube::client::Body::from(body.clone())).unwrap());
            // The first status check already sees it running.
            let (_, send) = api.next_request().await.unwrap();
            send.send_response(http::Response::builder().body(kube::client::Body::from(body)).unwrap());
        });
        let Reply::Debug { name, created, .. } = reply(&k, id) else { panic!() };
        assert!(created && name.starts_with("rtop-app-"));
    }
}
