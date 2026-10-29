//! Which container a process belongs to, from its `/proc/<pid>/cgroup`
//! path (research R4, R9).
//!
//! The container runtime puts every container in its own cgroup and names
//! the directory after the container ID:
//!
//! ```text
//! 0::/kubepods.slice/kubepods-burstable.slice/kubepods-burstable-pod<uid>.slice/cri-containerd-<id>.scope
//! 0::/kubelet.slice/.../cri-containerd-<id>.scope      (kind, systemd driver)
//! 0::/kubepods/burstable/pod<uid>/<id>                 (cgroupfs driver)
//! 0::/../cri-containerd-<id>.scope                     (seen from inside a cgroup namespace)
//! ```
//!
//! The same ID is in the pod's `status.containerStatuses[].containerID`.
//! Real lines from kind and other runtimes are in
//! `tests/fixtures/kube/cgroup-paths.txt`.

use std::collections::HashMap;
use std::sync::Arc;

use crate::model::Origin;

/// The 64-hex container ID at the end of the cgroup v2 path, if any.
pub fn container_id(cgroup: &str) -> Option<&str> {
    let path = cgroup.lines().find_map(|l| l.strip_prefix("0::"))?.trim_end();
    let last = path.rsplit('/').next()?;
    let last = last.strip_suffix(".scope").unwrap_or(last);
    // cri-containerd-<id>, crio-<id>, docker-<id>, or the bare ID.
    let id = last.rsplit('-').next()?;
    (id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit())).then_some(id)
}

/// The pod UID in a kubepods path, with the dashes the systemd driver
/// turns into underscores put back. Static pods (etcd, kube-apiserver...)
/// have a 32-hex config hash here instead, which their mirror pod carries
/// in the `kubernetes.io/config.mirror` annotation.
pub fn pod_uid(cgroup: &str) -> Option<String> {
    let path = cgroup.lines().find_map(|l| l.strip_prefix("0::"))?;
    // `kubepods` itself contains "pod": take the segment where a UID follows.
    path.split('/').find_map(|s| {
        let s = s.strip_suffix(".slice").unwrap_or(s);
        let uid = s[s.rfind("pod")? + 3..].replace('_', "-");
        let hex = |n: usize| uid.len() == n && uid.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-');
        (hex(36) || (hex(32) && !uid.contains('-'))).then_some(uid)
    })
}

/// Who owns the processes a reader sees, by cgroup: the containers of a
/// pod with a shared PID namespace, or every pod on a node.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Owners {
    /// Container ID → its pod and name.
    pub containers: HashMap<String, Origin>,
    /// Pod UID (or static pod hash) → `namespace/name`, for the pause
    /// container, which has no entry in the pod status.
    pub pods: HashMap<String, Arc<str>>,
    /// Label when nothing matches: `host` on a node, `?` in a pod.
    pub unmatched: Arc<str>,
}

impl Owners {
    /// The owner of a process with this cgroup file and command name.
    /// `own` is the reader's own container: inside its cgroup namespace
    /// its own cgroup reads as `/`.
    pub fn owner(&self, cgroup: &str, name: &str, own: &Origin) -> Origin {
        if cgroup.lines().any(|l| l.trim_end() == "0::/") {
            return own.clone();
        }
        if let Some(o) = container_id(cgroup).and_then(|id| self.containers.get(id)) {
            return o.clone();
        }
        let pause = |pod| Origin { pod, container: "pause".into() };
        // `/../<scope>` sits next to the reader's own container, in its pod:
        // the path has no pod segment, so a pause there is the reader's pod's.
        let path = cgroup.lines().find_map(|l| l.strip_prefix("0::")).unwrap_or_default().trim_end();
        let sibling = path.strip_prefix("/../").is_some_and(|rest| !rest.is_empty() && !rest.contains('/'));
        match pod_uid(cgroup).and_then(|uid| self.pods.get(&uid)) {
            Some(pod) => pause(Some(pod.clone())),
            None if name == "pause" => pause(if sibling { own.pod.clone() } else { None }),
            None => Origin { pod: None, container: self.unmatched.clone() },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "4f1c3a0e5b2d47f8a9c6e1b3d5f7a9c2e4b6d8f0a1c3e5b7d9f1a3c5e7b9d1f3";

    #[test]
    fn systemd_driver_path() {
        let cg = format!(
            "0::/kubelet.slice/kubelet-kubepods.slice/kubelet-kubepods-besteffort.slice/kubelet-kubepods-besteffort-pod1b2c3d4e_5f60_4a7b_8c9d_0e1f2a3b4c5d.slice/cri-containerd-{ID}.scope\n"
        );
        assert_eq!(container_id(&cg), Some(ID));
        assert_eq!(pod_uid(&cg).as_deref(), Some("1b2c3d4e-5f60-4a7b-8c9d-0e1f2a3b4c5d"));
    }

    #[test]
    fn cgroupfs_and_namespaced_paths() {
        assert_eq!(
            container_id(&format!("0::/kubepods/burstable/pod1b2c3d4e-5f60-4a7b-8c9d-0e1f2a3b4c5d/{ID}\n")),
            Some(ID)
        );
        assert_eq!(container_id(&format!("0::/../cri-containerd-{ID}.scope\n")), Some(ID));
    }

    /// Every line of the fixture, real ones from kind included.
    #[test]
    fn fixture_paths() {
        let text =
            std::fs::read_to_string(format!("{}/tests/fixtures/kube/cgroup-paths.txt", env!("CARGO_MANIFEST_DIR")))
                .unwrap();
        let mut n = 0;
        for line in text.lines().filter(|l| !l.is_empty() && !l.starts_with('#')) {
            let mut it = line.splitn(4, ' ');
            let (kind, pod, ctr, path) =
                (it.next().unwrap(), it.next().unwrap(), it.next().unwrap(), it.next().unwrap());
            let want = |s: &str| (s != "-").then(|| s.to_string());
            let cg = format!("{path}\n");
            assert_eq!(container_id(&cg).map(str::to_string), want(ctr), "{line}");
            assert_eq!(pod_uid(&cg), want(pod), "{line}");
            assert_eq!(kind == "host" || kind == "own", container_id(&cg).is_none(), "{line}");
            n += 1;
        }
        assert!(n >= 14);
    }

    #[test]
    fn owners_on_a_node() {
        let pod: Arc<str> = "kube-system/kindnet-r825t".into();
        let o = Owners {
            containers: [(ID.to_string(), Origin { pod: Some(pod.clone()), container: "kindnet-cni".into() })].into(),
            pods: [("16796b5d-f12f-404c-bd16-26bf8ba24751".to_string(), pod.clone())].into(),
            unmatched: "host".into(),
        };
        let own = Origin { pod: Some("rtop-demo/rtop-node-x".into()), container: "reader".into() };
        let c = o.owner(
            &format!("0::/../kubepods-pod16796b5d_f12f_404c_bd16_26bf8ba24751.slice/cri-containerd-{ID}.scope\n"),
            "kindnetd",
            &own,
        );
        assert_eq!((c.pod.as_deref(), c.container.as_ref()), (Some("kube-system/kindnet-r825t"), "kindnet-cni"));
        let p = o.owner("0::/../kubepods-pod16796b5d_f12f_404c_bd16_26bf8ba24751.slice/cri-containerd-0000000000000000000000000000000000000000000000000000000000000000.scope\n", "pause", &own);
        assert_eq!((p.pod.as_deref(), p.container.as_ref()), (Some("kube-system/kindnet-r825t"), "pause"));
        assert_eq!(
            o.owner("0::/../../system.slice/containerd.service\n", "containerd", &own).container.as_ref(),
            "host"
        );
        assert_eq!(o.owner("0::/\n", "sh", &own), own);
        // The helper's own pause container sits next to the reader.
        let sibling = o.owner(
            "0::/../cri-containerd-1111111111111111111111111111111111111111111111111111111111111111.scope\n",
            "pause",
            &own,
        );
        assert_eq!((sibling.pod, sibling.container.as_ref()), (own.pod.clone(), "pause"));
    }

    #[test]
    fn host_processes_have_none() {
        assert_eq!(container_id("0::/system.slice/containerd.service\n"), None);
        assert_eq!(container_id("0::/\n"), None);
        assert_eq!(pod_uid("0::/init.scope\n"), None);
    }
}
