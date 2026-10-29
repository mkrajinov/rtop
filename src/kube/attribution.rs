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
/// turns into underscores put back.
pub fn pod_uid(cgroup: &str) -> Option<String> {
    let path = cgroup.lines().find_map(|l| l.strip_prefix("0::"))?;
    // `kubepods` itself contains "pod": take the segment where a UID follows.
    path.split('/').find_map(|s| {
        let s = s.strip_suffix(".slice").unwrap_or(s);
        let uid = s[s.rfind("pod")? + 3..].replace('_', "-");
        (uid.len() == 36 && uid.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')).then_some(uid)
    })
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

    #[test]
    fn host_processes_have_none() {
        assert_eq!(container_id("0::/system.slice/containerd.service\n"), None);
        assert_eq!(container_id("0::/\n"), None);
        assert_eq!(pod_uid("0::/init.scope\n"), None);
    }
}
