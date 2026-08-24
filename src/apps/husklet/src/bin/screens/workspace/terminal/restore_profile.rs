use super::*;
use std::cell::RefCell;

struct OfflineLauncher {
    events: RefCell<Vec<(RestoreEvent, std::time::Instant)>>,
}

impl PaneLauncher for OfflineLauncher {
    fn spawn(
        &self,
        terminal: &vte4::Terminal,
        _argv: &[&str],
        _environment: &[&str],
    ) -> std::io::Result<(i32, vte4::Pty)> {
        // A real host PTY and deterministic child keep this characterization
        // independent of OCI, the daemon, and the network.
        PtyProcess::spawn(
            terminal,
            &[
                "/bin/sh",
                "-c",
                "printf 'HL_RESTORE_PROMPT_%s\\n' \"$$\"; exec /bin/sh -c 'while read line; do :; done'",
            ],
            &["PATH=/usr/bin:/bin"],
        )
    }

    fn observe(&self, event: RestoreEvent) {
        self.events.borrow_mut().push((event, std::time::Instant::now()));
    }
}

/// Profile characterization rather than a merge gate: synchronous restore
/// currently completes all pane launches before GTK can service the first
/// frame callback. Removing `ignore` is intentionally red until restoration
/// is made incremental.
#[test]
#[ignore = "profile: synchronous restore blocks the first frame through every pane launch"]
fn saved_layouts_reach_their_foreign_ptys_before_the_first_frame() {
    assert!(
        crate::test_support::on_the_toolkit_thread(|| {
            for panes in [1, 8, 32] {
                characterize(panes);
            }
        }),
        "restore profile requires an Xvfb display"
    );
}

fn characterize(panes: usize) {
    let pty_baseline = open_pty_count();
    let workspace = WorkspaceConfig::new(
        &format!("restore-profile-{}-{panes}", std::process::id()),
        "offline.invalid/unused",
        hl_ws::Arch::Amd64,
    );
    let tw = Window::bench(&workspace);
    let launcher = OfflineLauncher {
        events: RefCell::new(Vec::new()),
    };
    let first_frame = Rc::new(Cell::new(false));
    let observed = first_frame.clone();
    tw.stack.add_tick_callback(move |_, _| {
        observed.set(true);
        glib::ControlFlow::Break
    });

    let storage = workspace.storage_dir(&Home::current().root());
    std::fs::create_dir_all(Session::dir(&storage)).unwrap();
    for index in 0..panes {
        std::fs::write(
            Session::dir(&storage).join(format!("profile-{index}.txt")),
            format!("HL_RESTORE_HISTORY_{index}\n"),
        )
        .unwrap();
    }
    let session = Session {
        tabs: vec![SessionTab {
            title: format!("{panes} panes"),
            root: layout(panes, 0),
        }],
    };
    WindowSession::new(&tw).restore_with(&session, &launcher);
    let frame_ran_before_restore_returned = first_frame.get();

    let events = launcher.events.borrow();
    assert_eq!(events.first().map(|event| event.0), Some(RestoreEvent::Started));
    assert_eq!(events.last().map(|event| event.0), Some(RestoreEvent::Completed));
    let pids: Vec<i32> = events
        .iter()
        .filter_map(|(event, _)| match event {
            RestoreEvent::PaneStarted(pid) => Some(*pid),
            _ => None,
        })
        .collect();
    let starts: Vec<std::time::Instant> = events
        .iter()
        .filter_map(|(event, at)| (*event == RestoreEvent::PaneStarting).then_some(*at))
        .collect();
    let started: Vec<std::time::Instant> = events
        .iter()
        .filter_map(|(event, at)| matches!(event, RestoreEvent::PaneStarted(_)).then_some(*at))
        .collect();
    let total = events.last().unwrap().1.duration_since(events.first().unwrap().1);
    let launches: Vec<u128> = starts
        .iter()
        .zip(&started)
        .map(|(start, end)| end.duration_since(*start).as_micros())
        .collect();
    assert_eq!(pids.len(), panes);
    assert_eq!(tw.panes.borrow().len(), panes);
    assert_eq!(tw.pids.borrow().values().map(Vec::len).sum::<usize>(), panes);
    assert_eq!(launches.len(), panes);
    println!(
        "restore-profile panes={panes} total_us={} pane_launch_us={launches:?}",
        total.as_micros()
    );
    await_prompts(&tw, panes);
    for (index, pane) in tw.panes.borrow().iter().enumerate() {
        let terminal = pane.terminal.upgrade().unwrap();
        let text = Terminal::new(&terminal).history();
        assert!(text.contains(&format!("HL_RESTORE_HISTORY_{index}")));
        assert!(text.contains("HL_RESTORE_PROMPT_"));
    }

    for pid in pids {
        let status = std::process::Command::new("/bin/kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .expect("run the host signal utility");
        assert!(status.success(), "terminate restore-profile child {pid}");
    }
    tw.closing.set(true);
    let root = tw.stack.root().and_then(|root| root.downcast::<gtk::Window>().ok());
    if let Some(root) = root {
        root.close();
    }
    while let Some(child) = tw.stack.first_child() {
        tw.stack.remove(&child);
    }
    await_cleanup(&tw, pty_baseline);
    let _ = std::fs::remove_dir_all(Session::dir(&storage));
    assert!(
        frame_ran_before_restore_returned,
        "characterization: first frame did not run before pane 2 of {panes}"
    );
}

fn await_cleanup(window: &TermWin, pty_baseline: usize) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        while glib::MainContext::default().iteration(false) {}
        let children_reaped = window.pids.borrow().values().flatten().all(|pid| pid.get() == 0);
        if children_reaped && open_pty_count() <= pty_baseline {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "restore fixture leaked child or PTY: live_pids={} ptys={} baseline={pty_baseline}",
            window
                .pids
                .borrow()
                .values()
                .flatten()
                .filter(|pid| pid.get() != 0)
                .count(),
            open_pty_count()
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn open_pty_count() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| std::fs::read_link(entry.path()).ok())
        .filter(|path| path == std::path::Path::new("/dev/pts/ptmx"))
        .count()
}

fn await_prompts(window: &TermWin, panes: usize) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        while glib::MainContext::default().iteration(false) {}
        let ready = window.panes.borrow().iter().all(|pane| {
            pane.terminal
                .upgrade()
                .is_some_and(|terminal| Terminal::new(&terminal).history().contains("HL_RESTORE_PROMPT_"))
        });
        if ready {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{panes}-pane prompt marker timed out"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn layout(leaves: usize, base: usize) -> PaneNode {
    if leaves == 1 {
        return PaneNode::Leaf(Pane {
            cwd: None,
            history_file: Some(format!("profile-{base}.txt")),
            slot: Some(format!("profile-{base}")),
        });
    }
    let left = leaves / 2;
    PaneNode::Split {
        dir: if base.is_multiple_of(2) {
            SplitDir::Horizontal
        } else {
            SplitDir::Vertical
        },
        ratio: 0.5,
        a: Box::new(layout(left, base)),
        b: Box::new(layout(leaves - left, base + left)),
    }
}
