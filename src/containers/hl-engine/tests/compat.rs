use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

static SCRATCH: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
struct Case {
    name: String,
    isa: String,
    exit: i32,
    golden: PathBuf,
}

#[test]
fn bootstrap_matrix() {
    let root = corpus();
    let worker = env!("CARGO_BIN_EXE_hl-compat-worker");
    for case in cases(&root) {
        run(&worker, &root, &case);
    }
}

#[test]
fn repeated_inventory() {
    let root = corpus();
    let cases = cases(&root);
    assert_eq!(cases.len(), 14);
    let worker = env!("CARGO_BIN_EXE_hl-compat-worker");
    for _ in 0..2 {
        run(worker, &root, &cases[0]);
    }
}

#[test]
fn namespace_identity() {
    let root = corpus();
    let worker = env!("CARGO_BIN_EXE_hl-compat-worker");
    for isa in ["aarch64", "x86_64"] {
        run_guest(
            worker,
            root.join("prebuilt").join(isa).join("namespace"),
            &Case {
                name: "namespace".into(),
                isa: isa.into(),
                exit: 0,
                golden: root.join("golden/namespace.out"),
            },
            Some("scratch-rootfs"),
        );
    }
}

#[test]
fn exec_lifecycle() {
    let root = corpus();
    let worker = env!("CARGO_BIN_EXE_hl-compat-worker");
    for (suite, name, golden) in [
        ("process", "exec-cloexec-signal", "exec_cloexec_signal.out"),
        ("process", "exec-distinct", "exec_distinct.out"),
        ("process", "exec-errno", "exec_errno.out"),
        ("process", "exec-self", "exec_self.out"),
        ("procfs", "pf-auxv-execfn", "shared/auxv_execfn.out"),
        ("syscall", "rng-quality", "rng_quality.out"),
        ("threads", "exec-thread-binding", "exec_thread_binding.out"),
    ] {
        for isa in ["aarch64", "x86_64"] {
            let case = Case {
                name: name.into(),
                isa: isa.into(),
                exit: 0,
                golden: root
                    .join("oracle/tests/compat")
                    .join(suite)
                    .join("expected")
                    .join(golden),
            };
            let guest = root.join("artifacts/full").join(suite).join(isa).join(name);
            run_guest(worker, guest, &case, None);
        }
    }
}

#[test]
fn exit_stress() {
    let root = corpus();
    let worker = env!("CARGO_BIN_EXE_hl-compat-worker");
    for isa in ["aarch64", "x86_64"] {
        let case = Case {
            name: "exit".into(),
            isa: isa.into(),
            exit: 42,
            golden: root.join("golden/empty.out"),
        };
        let guest = root.join("prebuilt").join(isa).join("exit");
        for _ in 0..4 {
            run_guest(worker, guest.clone(), &case, None)
        }
    }
}

#[test]
fn failed_launch_cleanup() {
    let engine = hl_engine::runtime::Builder::new(
        hl_engine::activation::GuestIsa::X86_64,
        corpus().join("golden/empty.out"),
    )
    .with_input(hl_engine::runtime::Input::File {
        source: corpus().join("golden/status.out"),
        relative: PathBuf::from("fixtures/status.out"),
        executable: false,
    })
    .build()
    .unwrap();
    let workspace = engine.workspace().to_owned();
    assert!(workspace.join("fixtures/status.out").is_file());
    engine.start().unwrap();
    assert!(engine.wait().is_err());
    drop(engine);
    assert!(!workspace.exists());
}

#[test]
fn rejects_input_traversal() {
    let result = hl_engine::runtime::Builder::new(
        hl_engine::activation::GuestIsa::X86_64,
        corpus().join("prebuilt/x86_64/exit"),
    )
    .with_input(hl_engine::runtime::Input::File {
        source: corpus().join("golden/status.out"),
        relative: PathBuf::from("../escape"),
        executable: false,
    })
    .build();
    assert!(result.is_err());
}

#[test]
fn rejects_symlink_escape() {
    let result = hl_engine::runtime::Builder::new(
        hl_engine::activation::GuestIsa::X86_64,
        corpus().join("prebuilt/x86_64/exit"),
    )
    .with_input(hl_engine::runtime::Input::Symlink {
        relative: PathBuf::from("entry"),
        target: PathBuf::from("../guest"),
    })
    .build();
    assert!(result.is_err());
}

#[test]
fn rejects_input_collision() {
    let guest = corpus().join("prebuilt/x86_64/exit");
    let result = hl_engine::runtime::Builder::new(hl_engine::activation::GuestIsa::X86_64, &guest)
        .with_input(hl_engine::runtime::Input::File {
            source: guest,
            relative: PathBuf::from("guest"),
            executable: true,
        })
        .build();
    assert!(result.is_err());
}

#[test]
fn rejects_rootfs_traversal() {
    let result = hl_engine::runtime::Builder::new(
        hl_engine::activation::GuestIsa::X86_64,
        corpus().join("prebuilt/x86_64/exit"),
    )
    .with_rootfs(hl_engine::runtime::Rootfs::scratch("../guest"))
    .build();
    assert!(result.is_err());
}

#[test]
fn rooted_metadata() {
    use hl_engine::activation::GuestIsa;
    use hl_engine::runtime::{Builder, Input, Rootfs};
    for (isa, name) in [(GuestIsa::Aarch64, "aarch64"), (GuestIsa::X86_64, "x86_64")] {
        let root = corpus();
        let guest = root.join("prebuilt").join(name).join("path_metadata");
        let rootfs = Rootfs::scratch("guest")
            .with_input(Input::File {
                source: root.join("golden/status.out"),
                relative: PathBuf::from("target"),
                executable: false,
            })
            .with_input(Input::Symlink {
                relative: PathBuf::from("link"),
                target: PathBuf::from("target"),
            });
        let engine = Builder::new(isa, guest).with_rootfs(rootfs).build().unwrap();
        let workspace = engine.workspace().to_owned();
        engine.start().unwrap();
        let exit = engine.wait().unwrap();
        engine.destroy().unwrap();
        assert_eq!(hl_engine::program::Program::exit_status(exit), 0, "{name}");
        assert!(!workspace.exists(), "{name} workspace leaked");
    }
}

#[test]
fn directory_enumeration() {
    use hl_engine::activation::GuestIsa;
    use hl_engine::runtime::{Builder, Rootfs};
    for (isa, name) in [(GuestIsa::Aarch64, "aarch64"), (GuestIsa::X86_64, "x86_64")] {
        let root = corpus();
        let guest = root.join("prebuilt").join(name).join("directory");
        let engine = Builder::new(isa, guest)
            .with_rootfs(Rootfs::scratch("guest"))
            .build()
            .unwrap();
        let workspace = engine.workspace().to_owned();
        engine.start().unwrap();
        let exit = engine.wait().unwrap();
        engine.destroy().unwrap();
        assert_eq!(hl_engine::program::Program::exit_status(exit), 0, "{name}");
        assert!(!workspace.exists(), "{name} workspace leaked");
    }
}

#[test]
fn positional_io() {
    use hl_engine::activation::GuestIsa;
    use hl_engine::runtime::{Builder, Rootfs};
    for (isa, name) in [(GuestIsa::Aarch64, "aarch64"), (GuestIsa::X86_64, "x86_64")] {
        let root = corpus();
        let guest = root.join("prebuilt").join(name).join("positional");
        let engine = Builder::new(isa, guest)
            .with_rootfs(Rootfs::scratch("guest"))
            .build()
            .unwrap();
        let workspace = engine.workspace().to_owned();
        engine.start().unwrap();
        let exit = engine.wait().unwrap();
        engine.destroy().unwrap();
        assert_eq!(hl_engine::program::Program::exit_status(exit), 0, "{name}");
        assert!(!workspace.exists(), "{name} workspace leaked");
    }
}

#[test]
fn unix_socketpair() {
    let root = corpus();
    let worker = env!("CARGO_BIN_EXE_hl-compat-worker");
    for name in ["aarch64", "x86_64"] {
        for (case, golden) in [
            ("socketpair", "net_socketpair.out"),
            ("msg-dontwait", "net_msg_dontwait.out"),
            ("net_sendmsg", "net_sendmsg.out"),
            ("scm-rights", "net_scm_rights.out"),
            ("net_unix", "net_unix.out"),
        ] {
            run_guest(
                worker,
                root.join("artifacts/full/network").join(name).join(case),
                &Case {
                    name: case.to_owned(),
                    isa: name.to_owned(),
                    exit: 0,
                    golden: root.join("oracle/tests/compat/network/expected").join(golden),
                },
                None,
            );
        }
    }
}

#[test]
fn epoll_eventfd() {
    let root = corpus();
    let worker = env!("CARGO_BIN_EXE_hl-compat-worker");
    for name in ["aarch64", "x86_64"] {
        run_guest(
            worker,
            root.join("prebuilt").join(name).join("epoll"),
            &Case {
                name: "epoll".to_owned(),
                isa: name.to_owned(),
                exit: 0,
                golden: root.join("golden/epoll.out"),
            },
            None,
        );
    }
}

#[test]
fn signalfd_events() {
    let root = corpus();
    let worker = env!("CARGO_BIN_EXE_hl-compat-worker");
    for name in ["aarch64", "x86_64"] {
        for (suite, case, golden) in [
            ("signals", "signalfd-basic", "signals/expected/signalfd_basic.out"),
            ("signals", "signalfd-state", "signals/expected/signalfd_state.out"),
            (
                "syscall_edges",
                "sc-shortread-signalfd",
                "syscall_edges/expected/shared/shortread_signalfd.out",
            ),
        ] {
            run_guest(
                worker,
                root.join("artifacts/full").join(suite).join(name).join(case),
                &Case {
                    name: case.to_owned(),
                    isa: name.to_owned(),
                    exit: 0,
                    golden: root.join("oracle/tests/compat").join(golden),
                },
                None,
            );
        }
        for case in ["signalfd_epoll", "signalfd_edges", "signalfd_fork"] {
            run_guest(
                worker,
                root.join("prebuilt").join(name).join(case),
                &Case {
                    name: case.to_owned(),
                    isa: name.to_owned(),
                    exit: 0,
                    golden: root.join("golden").join(format!("{case}.out")),
                },
                None,
            );
        }
    }
}

#[test]
fn process_groups() {
    let root = corpus();
    let worker = env!("CARGO_BIN_EXE_hl-compat-worker");
    for name in ["aarch64", "x86_64"] {
        for (suite, case, golden, rootfs) in [
            ("process", "pgroup-session", "process/expected/pgroup_session.out", None),
            ("posix", "setpgid", "posix/expected/setpgid.out", None),
            (
                "syscall_edges",
                "sc-kill-zero-group",
                "syscall_edges/expected/shared/kill_zero_group.out",
                Some("alpine-rootfs"),
            ),
        ] {
            run_guest(
                worker,
                root.join("artifacts/full").join(suite).join(name).join(case),
                &Case {
                    name: case.to_owned(),
                    isa: name.to_owned(),
                    exit: 0,
                    golden: root.join("oracle/tests/compat").join(golden),
                },
                rootfs,
            );
        }
        run_guest(
            worker,
            root.join("prebuilt").join(name).join("job_control"),
            &Case {
                name: "job_control".to_owned(),
                isa: name.to_owned(),
                exit: 0,
                golden: root.join("golden/job_control.out"),
            },
            None,
        );
    }
}

#[test]
fn static_exec() {
    let root = corpus();
    let worker = env!("CARGO_BIN_EXE_hl-compat-worker");
    for name in ["aarch64", "x86_64"] {
        run_guest(
            worker,
            root.join("artifacts/full/posix").join(name).join("execself"),
            &Case {
                name: "execself".to_owned(),
                isa: name.to_owned(),
                exit: 0,
                golden: root.join("oracle/tests/compat/posix/expected/execself.out"),
            },
            None,
        );
    }
}

#[test]
fn filesystem_stats() {
    let root = corpus();
    let worker = env!("CARGO_BIN_EXE_hl-compat-worker");
    for name in ["aarch64", "x86_64"] {
        run_guest(
            worker,
            root.join("artifacts/full/syscall").join(name).join("fstatfs"),
            &Case {
                name: "fstatfs".to_owned(),
                isa: name.to_owned(),
                exit: 0,
                golden: root.join("oracle/tests/compat/syscall/expected/fstatfs.out"),
            },
            Some("alpine-rootfs"),
        );
    }
}

#[test]
fn interval_timer() {
    let root = corpus();
    let worker = env!("CARGO_BIN_EXE_hl-compat-worker");
    for name in ["aarch64", "x86_64"] {
        for (case, golden) in [
            ("itimer-real", "itimer_real.out"),
            ("itimer-unarmed", "itimer_unarmed.out"),
        ] {
            run_guest(
                worker,
                root.join("artifacts/full/time").join(name).join(case),
                &Case {
                    name: case.to_owned(),
                    isa: name.to_owned(),
                    exit: 0,
                    golden: root.join("oracle/tests/compat/time/expected/shared").join(golden),
                },
                None,
            );
        }
    }
}

#[test]
fn file_size_sync() {
    let root = corpus();
    let worker = env!("CARGO_BIN_EXE_hl-compat-worker");
    for (name, group, case, golden) in [
        ("aarch64", "filesystem", "truncate-grow", "truncate_grow.out"),
        ("aarch64", "posix", "fsync", "fsync.out"),
        ("aarch64", "posix", "truncate", "truncate.out"),
        ("x86_64", "filesystem", "truncate-grow", "truncate_grow.out"),
        ("x86_64", "posix", "fsync", "fsync.out"),
        ("x86_64", "posix", "truncate", "truncate.out"),
    ] {
        let shared = (group == "filesystem").then_some("shared");
        run_guest(
            worker,
            root.join("artifacts/full").join(group).join(name).join(case),
            &Case {
                name: case.to_owned(),
                isa: name.to_owned(),
                exit: 0,
                golden: root
                    .join("oracle/tests/compat")
                    .join(group)
                    .join("expected")
                    .join(shared.unwrap_or(""))
                    .join(golden),
            },
            Some("alpine-rootfs"),
        );
    }
    for (name, case, rootfs, golden) in [
        ("aarch64", "truncate-eof-bus", "alpine-rootfs", "truncate_bus.out"),
        ("aarch64", "truncate-peer", "mapping-data-rootfs", "truncate_peer.out"),
        ("x86_64", "truncate-eof-bus", "alpine-rootfs", "truncate_bus.out"),
        ("x86_64", "truncate-peer", "mapping-data-rootfs", "truncate_peer.out"),
    ] {
        run_guest(
            worker,
            root.join("artifacts/full/memory").join(name).join(case),
            &Case {
                name: case.to_owned(),
                isa: name.to_owned(),
                exit: 0,
                golden: root.join("oracle/tests/compat/memory/expected").join(golden),
            },
            Some(rootfs),
        );
    }
}

#[test]
fn memory_control() {
    let root = corpus();
    let worker = env!("CARGO_BIN_EXE_hl-compat-worker");
    for name in ["aarch64", "x86_64"] {
        for (suite, case, golden, rootfs) in [
            (
                "memory",
                "msync-hole",
                "memory/expected/msync_hole.out",
                "alpine-rootfs",
            ),
            (
                "memory",
                "msync-writeback",
                "memory/expected/msync_writeback.out",
                "alpine-rootfs",
            ),
            (
                "memory",
                "madvise-hints",
                "memory/expected/madv_hints.out",
                "scratch-rootfs",
            ),
            (
                "memory",
                "madvise-dontneed-partial",
                "memory/expected/madv_dontneed_partial.out",
                "scratch-rootfs",
            ),
            (
                "memory",
                "madv-dontfork",
                "memory/expected/madv_dontfork.out",
                "scratch-rootfs",
            ),
            (
                "completeness",
                "mincore",
                "completeness/expected/shared/mincore.out",
                "scratch-rootfs",
            ),
            (
                "memory",
                "mapping-resize",
                "memory/expected/mremap.out",
                "scratch-rootfs",
            ),
            (
                "memory",
                "mremap-dontunmap",
                "memory/expected/mremap_dontunmap.out",
                "scratch-rootfs",
            ),
            (
                "completeness",
                "mremap",
                "completeness/expected/shared/mremap.out",
                "scratch-rootfs",
            ),
        ] {
            run_guest(
                worker,
                root.join("artifacts/full").join(suite).join(name).join(case),
                &Case {
                    name: case.to_owned(),
                    isa: name.to_owned(),
                    exit: 0,
                    golden: root.join("oracle/tests/compat").join(golden),
                },
                Some(rootfs),
            );
        }
    }
}

#[test]
fn socket_teardown_child() {
    let Ok(mode) = std::env::var("HL_SOCKET_TEARDOWN_CHILD") else {
        return;
    };
    let (isa, name) = match mode.as_str() {
        "aarch64" | "aarch64-isolation" => (hl_engine::activation::GuestIsa::Aarch64, "aarch64"),
        "x86_64" | "x86_64-isolation" => (hl_engine::activation::GuestIsa::X86_64, "x86_64"),
        _ => panic!("unknown teardown mode"),
    };
    let count = if mode.ends_with("isolation") { 2 } else { 1 };
    let mut engines = Vec::new();
    for _ in 0..count {
        let engine = hl_engine::runtime::Builder::new(isa, corpus().join("prebuilt").join(name).join("socket_stop"))
            .build()
            .unwrap();
        engine.start().unwrap();
        engines.push(engine);
    }
    thread::sleep(Duration::from_millis(100));
    for (index, engine) in engines.into_iter().enumerate() {
        let workspace = engine.workspace().to_owned();
        engine.stop(hl_engine::engine::StopRequest::Force).unwrap();
        let exit = engine.wait().unwrap();
        assert_eq!(exit.kind, hl_engine::engine::ExitKind::Signal);
        assert_eq!(exit.guest_status, 9);
        assert!(!workspace.exists(), "socket workspace leaked");
        engine.destroy().unwrap();
        if count == 2 && index == 0 {
            // Give an incorrectly process-global cancellation enough time to
            // terminate the unrelated engine before its own stop request.
            thread::sleep(Duration::from_millis(50));
        }
    }
}

#[test]
fn blocked_socket_teardown() {
    let executable = std::env::current_exe().unwrap();
    for mode in ["aarch64", "x86_64", "aarch64-isolation", "x86_64-isolation"] {
        run_teardown(&executable, mode);
    }
}

fn run_teardown(executable: &Path, mode: &str) {
    let mut child = Command::new(executable)
        .args(["--exact", "socket_teardown_child", "--nocapture"])
        .env("HL_SOCKET_TEARDOWN_CHILD", mode)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let started = Instant::now();
    loop {
        match child.try_wait().unwrap() {
            Some(status) => {
                assert!(status.success(), "socket teardown child failed: {mode}");
                return;
            }
            None if started.elapsed() < Duration::from_secs(3) => {
                thread::sleep(Duration::from_millis(5));
            }
            None => {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("socket teardown child timed out: {mode}");
            }
        }
    }
}

fn run(worker: &str, root: &Path, case: &Case) {
    run_guest(
        worker,
        root.join("prebuilt").join(&case.isa).join(&case.name),
        case,
        None,
    )
}

fn run_guest(worker: &str, guest: PathBuf, case: &Case, rootfs: Option<&str>) {
    let base = std::env::temp_dir().join(format!(
        "hl-compat-{}-{}-{}-{}",
        std::process::id(),
        case.name,
        case.isa,
        SCRATCH.fetch_add(1, Ordering::Relaxed),
    ));
    let report = base.join("result");
    let output = fs::File::create(base.join("stdout"))
        .or_else(|_| {
            fs::create_dir_all(&base)?;
            fs::File::create(base.join("stdout"))
        })
        .unwrap();
    let error = fs::File::create(base.join("stderr")).unwrap();
    let mut command = Command::new(worker);
    command.arg(&case.isa).arg(guest).arg("-").arg(&report);
    if let Some(rootfs) = rootfs {
        command.args(["executable", "-", "-", rootfs]);
    }
    let mut child = command
        .stdout(Stdio::from(output))
        .stderr(Stdio::from(error))
        .spawn()
        .unwrap();
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() >= Duration::from_secs(3) {
            child.kill().unwrap();
            child.wait().unwrap();
            let _ = fs::remove_dir_all(&base);
            panic!("{} {} timed out", case.name, case.isa);
        }
        thread::sleep(Duration::from_millis(5));
    };
    assert!(status.success(), "{} {} worker failed", case.name, case.isa);
    let report = fs::read_to_string(&report).unwrap();
    let mut report = report.lines();
    assert_eq!(report.next().unwrap().parse::<i32>().unwrap(), case.exit);
    assert_eq!(report.next(), Some("true"), "engine workspace leaked");
    assert_eq!(fs::read(base.join("stdout")).unwrap(), fs::read(&case.golden).unwrap());
    assert!(fs::read(base.join("stderr")).unwrap().is_empty());
    fs::remove_dir_all(base).unwrap();
}

fn corpus() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../tests/runtime")
}

fn cases(root: &Path) -> Vec<Case> {
    let manifest = fs::read_to_string(root.join("manifest.tsv")).unwrap();
    manifest
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .flat_map(|line| {
            let fields = line.split('\t').collect::<Vec<_>>();
            fields[2].split(',').map(move |isa| Case {
                name: fields[0].to_owned(),
                isa: isa.to_owned(),
                exit: fields[4].parse().unwrap(),
                golden: root.join(fields[5]),
            })
        })
        .collect()
}
