//! Snapshot tests of whole screens, rendered from the fixture collector so
//! they never depend on what is running on the test machine.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use rtop::app::App;
use rtop::collector::fixture::FixtureCollector;
use rtop::ui;

fn app() -> App {
    let mut app = App::new(Box::new(FixtureCollector::default()));
    app.refresh();
    app
}

fn render(app: &mut App, w: u16, h: u16) -> String {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| ui::draw(f, app)).unwrap();
    let buf = term.backend().buffer();
    (0..h)
        .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>().trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

fn press(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::from(code));
}

#[test]
fn process_table() {
    let mut app = app();
    insta::assert_snapshot!(render(&mut app, 130, 16));
}

#[test]
fn tree_view_shows_parentage() {
    let mut app = app();
    press(&mut app, KeyCode::Char('t'));
    let screen = render(&mut app, 130, 16);
    assert!(screen.contains("└─ "), "{screen}");
    insta::assert_snapshot!(screen);
}

#[test]
fn explain_panel_follows_column_focus() {
    let mut app = app();
    press(&mut app, KeyCode::Char('e'));
    for _ in 0..3 {
        press(&mut app, KeyCode::Right); // CPU% → MEM% → VSZ → RSS
    }
    let screen = render(&mut app, 150, 40);
    assert!(screen.contains("RSS — resident set size"), "{screen}");
    insta::assert_snapshot!(screen);
}

#[test]
fn detail_view_and_raw_jump() {
    let mut app = app();
    press(&mut app, KeyCode::Enter);
    let screen = render(&mut app, 120, 20);
    assert!(screen.contains("1 Overview"), "{screen}");
    insta::assert_snapshot!("detail_overview", screen);

    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down); // State
    press(&mut app, KeyCode::Char('r'));
    let screen = render(&mut app, 120, 20);
    insta::assert_snapshot!("detail_raw", screen);
}

#[test]
fn filter_narrows_rows() {
    let mut app = app();
    press(&mut app, KeyCode::Char('/'));
    for c in "hog".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.rows.len(), 1);
    assert_eq!(app.selected_process().unwrap().name, "cpuhog");
}

#[test]
fn theme_picker_previews_and_esc_restores() {
    let mut app = app();
    let before = rtop::ui::theme::current();
    press(&mut app, KeyCode::Char('T'));
    let screen = render(&mut app, 130, 30);
    assert!(screen.contains("neutral") && screen.contains("emerald"), "{screen}");
    press(&mut app, KeyCode::Down);
    assert_ne!(rtop::ui::theme::current(), before, "arrow keys apply live");
    press(&mut app, KeyCode::Esc);
    assert_eq!(rtop::ui::theme::current(), before);
}

/// Two containers of a pod each have a PID 1 and a PID 7. The tree must keep
/// them apart and hang each child under the parent from its own namespace.
#[test]
fn same_pid_in_two_namespaces_stays_apart() {
    use rtop::collector::fixture::process;
    let mut c = FixtureCollector::default();
    c.snapshot.processes.clear();
    for (space, root, child) in [(1, "app", "worker"), (2, "sidecar", "shipper")] {
        let mut p = process(1, 0, root);
        p.space = space;
        let mut k = process(7, 1, child);
        k.space = space;
        c.snapshot.processes.extend([p, k]);
    }
    let mut app = App::new(Box::new(c));
    app.refresh();
    press(&mut app, KeyCode::Char('t'));
    assert_eq!(app.rows.len(), 4);
    let names: Vec<_> = app
        .rows
        .iter()
        .map(|r| {
            let p = &app.snapshot.processes[r.idx];
            format!("{}{}@{}", r.prefix, p.name, p.space)
        })
        .collect();
    for (root, child, space) in [("app", "worker", 1), ("sidecar", "shipper", 2)] {
        let i = names.iter().position(|n| n == &format!("{root}@{space}")).unwrap();
        assert!(names[i + 1].ends_with(&format!("{child}@{space}")), "{names:?}");
    }
}

/// A pod with containers `app` and `sidecar`: CONTAINER column, the target
/// line above the cards, and `C` narrowing to one container.
#[test]
fn pod_mode_shows_containers_and_target() {
    use rtop::collector::fixture::process;
    use rtop::model::{Link, Origin, Target};
    let mut c = FixtureCollector::default();
    c.snapshot.processes.clear();
    for (space, container, cmd) in [(1, "app", "nginx -g daemon off;"), (2, "sidecar", "envoy -c /etc/envoy.yaml")] {
        let origin = Some(Origin { pod: None, container: container.into() });
        let mut root = process(1, 0, cmd.split(' ').next().unwrap());
        root.cmdline = cmd.into();
        root.space = space;
        root.origin = origin.clone();
        let mut child = process(7, 1, "worker");
        child.space = space;
        child.origin = origin;
        c.snapshot.processes.extend([root, child]);
    }
    c.target = Some(Target::Pod {
        context: "kind-rtop".into(),
        namespace: "rtop-demo".into(),
        pod: "multi".into(),
        node: Some("rtop-control-plane".into()),
        container: None,
    });
    c.link = Some(Link { implicit_context: false, stale: None, notes: vec![] });
    let mut app = App::new(Box::new(c));
    app.refresh();
    let screen = render(&mut app, 130, 16);
    assert!(screen.contains("kind-rtop · rtop-demo/multi · node rtop-control-plane"), "{screen}");
    assert!(screen.contains("CONTAINER"), "{screen}");
    insta::assert_snapshot!(screen);

    press(&mut app, KeyCode::Char('C'));
    assert_eq!(app.rows.len(), 2);
    assert!(app.rows.iter().all(|r| rtop::app::container(&app.snapshot.processes[r.idx]) == "app"));
    // Signals stay off without --allow-actions.
    press(&mut app, KeyCode::Char('k'));
    assert_eq!(app.mode, rtop::app::Mode::Table);
}

#[cfg(feature = "kube")]
#[test]
fn k8s_picker_lists_namespaces_and_pods() {
    use rtop::kube::msg::PodSummary;
    use rtop::picker::Picker;
    let mut p = Picker::new("kind-rtop", true, "v1.32.2", "rtop-demo");
    p.set_namespaces(vec!["default".into(), "kube-system".into(), "rtop-demo".into()]);
    let pod = |name: &str, phase: &str, reason: Option<&str>, ready, containers, restarts| PodSummary {
        ns: "rtop-demo".into(),
        name: name.into(),
        phase: phase.into(),
        reason: reason.map(Into::into),
        node: Some("rtop-control-plane".into()),
        containers,
        ready,
        restarts,
        created: Some(1_000_000 - 3 * 3600),
    };
    p.set_pods(Ok(vec![
        pod("distroless", "Running", None, 1, 1, 2),
        pod("limited", "Running", None, 1, 1, 0),
        pod("multi", "Running", None, 2, 2, 0),
        pod("broken", "Pending", Some("ImagePullBackOff"), 0, 1, 0),
    ]));
    p.now = 1_000_000;
    let mut term = Terminal::new(TestBackend::new(110, 12)).unwrap();
    term.draw(|f| ui::picker::draw(f, &p)).unwrap();
    let buf = term.backend().buffer();
    let screen = (0..12)
        .map(|y| (0..110).map(|x| buf[(x, y)].symbol()).collect::<String>().trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(screen.contains("kind-rtop (current context)"), "{screen}");
    assert!(screen.contains("ImagePullBackOff"), "{screen}");
    insta::assert_snapshot!(screen);
}

#[test]
fn long_container_names_are_not_cut() {
    use rtop::collector::fixture::process;
    use rtop::model::{Origin, Target};
    let mut c = FixtureCollector::default();
    c.snapshot.processes.clear();
    let mut p = process(1, 0, "sleep");
    p.space = 1;
    p.origin = Some(Origin { pod: None, container: "rtop-sb-shard-140-1a2b3".into() });
    c.snapshot.processes.push(p);
    c.target = Some(Target::Pod {
        context: "kind-rtop".into(),
        namespace: "rtop-demo".into(),
        pod: "sb".into(),
        node: None,
        container: None,
    });
    let mut app = App::new(Box::new(c));
    app.refresh();
    let screen = render(&mut app, 130, 16);
    assert!(screen.contains("rtop-sb-shard-140-1a2b3 "), "{screen}");
}

#[test]
fn node_mode_shows_pods_and_host() {
    use rtop::collector::fixture::process;
    use rtop::model::{Link, Origin, Target};
    let mut c = FixtureCollector::default();
    c.snapshot.processes.clear();
    for (pid, pod, container, cmd) in [
        (744, None, "host", "kubelet --config=/var/lib/kubelet/config.yaml"),
        (579, Some("kube-system/etcd-rtop-control-plane"), "etcd", "etcd --data-dir=/var/lib/etcd"),
        (942, Some("kube-system/kindnet-r825t"), "pause", "pause"),
        (1696, Some("rtop-demo/multi"), "app", "sleep 3600"),
    ] {
        let mut p = process(pid, 1, cmd.split(' ').next().unwrap());
        p.cmdline = cmd.into();
        p.origin = Some(Origin { pod: pod.map(Into::into), container: container.into() });
        c.snapshot.processes.push(p);
    }
    c.target = Some(Target::Node {
        context: "kind-rtop".into(),
        node: "rtop-control-plane".into(),
        detail: "allocatable 10 cpu 23.4G · 13 pods".into(),
    });
    c.link =
        Some(Link { implicit_context: false, stale: None, notes: vec!["via helper pod rtop-demo/rtop-node-x".into()] });
    let mut app = App::new(Box::new(c));
    app.refresh();
    let screen = render(&mut app, 150, 16);
    assert!(screen.contains("POD") && screen.contains("kube-system/etcd-rtop-control-plane"), "{screen}");
    insta::assert_snapshot!(screen);

    // `/` matches the pod name.
    press(&mut app, KeyCode::Char('/'));
    for ch in "multi".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.rows.len(), 1);
    // Node mode is read-only, whatever --allow-actions says.
    app.allow_actions = true;
    press(&mut app, KeyCode::Char('k'));
    assert_eq!(app.mode, rtop::app::Mode::Table);
}

#[cfg(feature = "kube")]
#[test]
fn k8s_picker_lists_nodes_and_confirms_the_helper() {
    use rtop::kube::msg::NodeSummary;
    use rtop::picker::{Action, Picker};
    let mut p = Picker::new("kind-rtop", false, "v1.32.2", "rtop-demo");
    p.set_namespaces(vec!["default".into(), "rtop-demo".into()]);
    p.came_back_from_node("worker-1");
    let node = |name: &str, ready, roles: &[&str], unschedulable| NodeSummary {
        name: name.into(),
        ready,
        roles: roles.iter().map(|r| r.to_string()).collect(),
        created: Some(1_000_000 - 5 * 86_400),
        version: "v1.32.2".into(),
        cpu: Some("8".into()),
        memory: Some("32827316Ki".into()),
        unschedulable,
    };
    p.set_nodes(Ok(vec![
        node("control-plane", true, &["control-plane"], false),
        node("worker-1", true, &[], false),
        node("worker-2", true, &[], true),
        node("worker-3", false, &[], false),
    ]));
    p.now = 1_000_000;
    p.handle_key(KeyEvent::from(KeyCode::Enter));
    let mut term = Terminal::new(TestBackend::new(110, 10)).unwrap();
    term.draw(|f| ui::picker::draw(f, &p)).unwrap();
    let buf = term.backend().buffer();
    let screen = (0..10)
        .map(|y| (0..110).map(|x| buf[(x, y)].symbol()).collect::<String>().trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(screen.contains("create a hostPID helper pod in kind-rtop / rtop-demo on node worker-1?"), "{screen}");
    assert!(screen.contains("Ready,SchedulingDisabled"), "{screen}");
    insta::assert_snapshot!(screen);
    assert_eq!(p.handle_key(KeyEvent::from(KeyCode::Char('y'))), Action::OpenNode { node: "worker-1".into() });
}

#[test]
fn pod_mode_containers_card_shows_limits_throttling_and_ooms() {
    use rtop::model::{ContainerUsage, Target};
    let mut c = FixtureCollector {
        target: Some(Target::Pod {
            context: "kind-rtop".into(),
            namespace: "rtop-demo".into(),
            pod: "web".into(),
            node: Some("rtop-control-plane".into()),
            container: None,
        }),
        ..Default::default()
    };
    c.snapshot.containers = vec![
        ContainerUsage {
            name: "app".into(),
            cpu_cores: Some(0.5),
            cpu_limit: Some(0.5),
            throttled_pct: Some(97.0),
            mem_current: Some(200 << 20),
            mem_max: Some(256 << 20),
            oom_kills: Some(1),
            restarts: 3,
            last_termination: Some("terminated: OOMKilled (exit 137)".into()),
            source: "[web/app] /sys/fs/cgroup/".into(),
            ..Default::default()
        },
        ContainerUsage {
            name: "sidecar".into(),
            cpu_cores: Some(0.01),
            mem_current: Some(12 << 20),
            oom_kills: Some(0),
            source: "[web/sidecar] /sys/fs/cgroup/".into(),
            ..Default::default()
        },
        ContainerUsage {
            name: "logger".into(),
            cpu_limit: Some(0.1),
            mem_max: Some(64 << 20),
            unreadable: true,
            source: "container is waiting: CrashLoopBackOff".into(),
            ..Default::default()
        },
    ];
    let mut app = App::new(Box::new(c));
    app.refresh();
    let screen = render(&mut app, 170, 24);
    for want in [
        "memory · node",
        "0.50 / 0.5",
        "thr  97% ",
        "200M / 256M",
        "oom 1",
        "↻ 3 last OOMKilled",
        "· no limit",
        "limits cpu 0.1 mem 64.0M",
    ] {
        assert!(screen.contains(want), "{want}\n{screen}");
    }
    insta::assert_snapshot!(screen);

    // L walks through the lessons in the explain panel.
    press(&mut app, KeyCode::Char('L'));
    assert!(app.show_explain);
    assert_eq!(app.explain_key(), "container_meminfo");
    press(&mut app, KeyCode::Char('L'));
    assert_eq!(app.explain_key(), "k8s_limits");
    press(&mut app, KeyCode::Right);
    assert_ne!(app.explain_key(), "k8s_limits");
}
