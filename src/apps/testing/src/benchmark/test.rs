use super::{Isa, Phase, Provider, Run, Summary, adapter};
use std::ffi::OsStr;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn command_run(provider: Provider, binary: PathBuf, engine: Option<PathBuf>) -> Run {
    Run {
        provider,
        isa: Isa::X86,
        binary,
        rootfs: None,
        engine,
        c_runner: None,
        output: None,
        repeats: 1,
        timeout: Duration::from_secs(1),
        guest: vec!["--phase".into(), "compute".into()],
        environment: vec![("BENCH_SEED".into(), "7".into())],
    }
}

#[test]
fn phase_contract() {
    assert!(matches!(
        Phase::parse("PHASE compute us=42 ok=7"),
        Ok(Some((name, Phase { time: 42, checksum: 7 }))) if name == "compute"
    ));
    assert!(Phase::parse("PHASE file us=100 ok=0").is_err());
}

#[test]
fn the_timebase_verdict_refuses_only_a_clock_no_correct_engine_could_report() {
    assert!(super::timebase_verdict("timebase", 101_848, 1).is_ok());
    // A busy box only lengthens the sleep, so slow arms stay admitted.
    assert!(super::timebase_verdict("timebase", 4_000_000, 1).is_ok());
    assert!(super::timebase_verdict("timebase", 2615, 21).is_err());
    assert!(super::timebase_verdict("timebase", 2440, 1).is_err());
    // Work-counting phases keep their own contract; this one owns no opinion of them.
    assert!(super::timebase_verdict("compute", 2, 441_094_035_400_083_178).is_ok());
}

#[test]
fn median_contract() {
    assert_eq!(Summary::median(&mut [9, 1, 5]), 5);
    assert_eq!(Summary::median(&mut [9, 1, 5, 3]), 4);
}

#[test]
fn repeat_summary_rejects_divergent_syscall_checksums() {
    let mut summary = Summary::default();
    summary.observe("syscall", Phase { time: 10, checksum: 42 }).unwrap();
    assert_eq!(
        summary
            .observe("syscall", Phase { time: 11, checksum: 43 })
            .unwrap_err(),
        "checksum changed across repeats for syscall"
    );
}

#[test]
fn engine_settings_are_rejected_where_they_would_not_apply() {
    for name in ["HL_NATIVE_EXECUTION", "HL_A64_DIRTY_OVERFLOW_CONTINUE"] {
        let mut run = command_run(Provider::Native, "/bin/sh".into(), None);
        run.environment = vec![(name.into(), "1".into())];
        assert_eq!(
            run.validate().unwrap_err(),
            format!("{name} is honoured only as --engine-option, not --env")
        );
    }
}

#[test]
fn provider_commands() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let binary = directory.path().join("guest");
    let engine = directory.path().join("engine");
    fs::write(&binary, []).expect("guest fixture");
    fs::write(&engine, []).expect("engine fixture");

    let process = adapter::Process::new(None);
    let native = process
        .command(&command_run(Provider::Native, binary.clone(), None))
        .expect("native command");
    assert_eq!(native.get_program(), binary.as_os_str());
    assert_eq!(
        native.get_args().collect::<Vec<_>>(),
        [OsStr::new("--phase"), OsStr::new("compute")]
    );
    assert!(
        native
            .get_envs()
            .any(|(name, value)| { name == OsStr::new("BENCH_SEED") && value == Some(OsStr::new("7")) })
    );

    let qemu = process
        .command(&command_run(Provider::Qemu, binary.clone(), None))
        .expect("qemu command");
    assert_eq!(qemu.get_program(), OsStr::new("qemu-x86_64"));
    assert_eq!(
        qemu.get_args().collect::<Vec<_>>(),
        [binary.as_os_str(), OsStr::new("--phase"), OsStr::new("compute")]
    );

    let c = process
        .command(&command_run(Provider::C, binary.clone(), Some(engine.clone())))
        .expect("C engine command");
    assert_eq!(c.get_program(), engine.as_os_str());
    assert_eq!(
        c.get_args().collect::<Vec<_>>(),
        [binary.as_os_str(), OsStr::new("--phase"), OsStr::new("compute")]
    );

    let wrapper = directory.path().join("runner");
    fs::write(&wrapper, b"prefix ENGINE GUEST [args...] suffix").unwrap();
    let mut wrapped = command_run(Provider::C, binary.clone(), Some(engine.clone()));
    wrapped.c_runner = Some(wrapper.clone());
    let command = process.command(&wrapped).expect("typed C runner command");
    assert_eq!(command.get_program(), wrapper.as_os_str());
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        [
            engine.as_os_str(),
            binary.as_os_str(),
            OsStr::new("--phase"),
            OsStr::new("compute"),
        ]
    );

    let rejected = command_run(Provider::C, binary, Some(wrapper));
    assert!(
        process
            .command(&rejected)
            .unwrap_err()
            .contains("exec wrapper configured as --engine")
    );
}

#[test]
fn providers_project_one_rootfs_without_host_execution_shortcuts() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let rootfs = directory.path().join("rootfs");
    let binary = rootfs.join("bin/true");
    let engine = directory.path().join("engine");
    fs::create_dir_all(binary.parent().unwrap()).unwrap();
    fs::write(&binary, []).unwrap();
    fs::write(&engine, []).unwrap();
    let process = adapter::Process::new(None);
    let rooted = |provider, selected_engine| {
        let mut run = command_run(provider, PathBuf::from("/bin/true"), selected_engine);
        run.rootfs = Some(rootfs.clone());
        run.validate().unwrap()
    };
    let native = process.command(&rooted(Provider::Native, None)).unwrap();
    assert_eq!(native.get_program(), binary.as_os_str());
    let qemu = process.command(&rooted(Provider::Qemu, None)).unwrap();
    assert_eq!(
        qemu.get_args().collect::<Vec<_>>(),
        [
            OsStr::new("-L"),
            rootfs.as_os_str(),
            binary.as_os_str(),
            OsStr::new("--phase"),
            OsStr::new("compute")
        ]
    );
    let command = process.command(&rooted(Provider::C, Some(engine.clone()))).unwrap();
    let arguments = command.get_args().collect::<Vec<_>>();
    assert!(
        arguments
            .windows(2)
            .any(|pair| pair == [OsStr::new("--rootfs"), rootfs.as_os_str()])
    );
    assert!(arguments.iter().any(|argument| *argument == OsStr::new("/bin/true")));
    let wrapper = directory.path().join("runner");
    fs::write(&wrapper, b"ENGINE GUEST [args...]").unwrap();
    let mut wrapped = rooted(Provider::C, Some(engine.clone()));
    wrapped.c_runner = Some(wrapper.clone());
    let command = process.command(&wrapped).unwrap();
    assert_eq!(command.get_program(), wrapper.as_os_str());
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        [
            engine.as_os_str(),
            OsStr::new("--rootfs"),
            rootfs.as_os_str(),
            OsStr::new("/bin/true"),
            OsStr::new("--phase"),
            OsStr::new("compute")
        ]
    );
}

#[test]
fn rootfs_rejects_unconfined_guest_paths() {
    let directory = tempfile::tempdir().unwrap();
    let mut run = command_run(Provider::Native, PathBuf::from("bin/true"), None);
    run.rootfs = Some(directory.path().to_path_buf());
    assert_eq!(
        run.validate().unwrap_err(),
        "rootfs guest executable must be an absolute confined path"
    );
}

#[cfg(unix)]
#[test]
fn timeout_contract() {
    let run = Run {
        provider: Provider::Native,
        isa: Isa::Aarch64,
        binary: PathBuf::from("/bin/sh"),
        rootfs: None,
        engine: None,
        c_runner: None,
        output: None,
        repeats: 1,
        timeout: Duration::from_secs(1),
        guest: vec!["-c".into(), "sleep 30 & echo 'PHASE wait us=1 ok=1'; wait".into()],
        environment: Vec::new(),
    };
    let started = Instant::now();
    assert!(adapter::Process::new(None).sample(&run).is_err());
    assert!(started.elapsed() < Duration::from_secs(3));
}
