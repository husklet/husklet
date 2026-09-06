#![cfg(all(target_os = "linux", feature = "native-test-hooks"))]

use std::{
    ffi::CString,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::Path,
    process::{Command, Stdio},
};
use tempfile::TempDir;

fn classify(proc_root: &Path, pid: i32, private_fds: &[i32]) -> i32 {
    let root = CString::new(proc_root.as_os_str().as_bytes()).unwrap();
    hl_native::native_checkpoint_admission_test(&root, pid, private_fds)
}

fn wait_for_mapping(pid: i32, needle: &str) {
    for _ in 0..1000 {
        if std::fs::read_to_string(format!("/proc/{pid}/maps")).is_ok_and(|maps| maps.contains(needle)) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    panic!("child {pid} did not map {needle}");
}

fn harness_private_fds(pid: i32) -> Vec<i32> {
    std::fs::read_dir(format!("/proc/{pid}/fd"))
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let fd = entry.file_name().to_string_lossy().parse::<i32>().ok()?;
            let target = std::fs::read_link(entry.path()).ok()?;
            (fd > 2 && target == Path::new("/var/tmp/husklet-box.lock")).then_some(fd)
        })
        .collect()
}

fn live_fixture(work: &Path) -> std::path::PathBuf {
    let source = work.join("wait.c");
    let executable = work.join("wait");
    std::fs::write(&source, b"#include <unistd.h>\nint main(void){for(;;) pause();}\n").unwrap();
    #[cfg(target_arch = "x86_64")]
    let compiler = "x86_64-linux-gnu-gcc";
    #[cfg(target_arch = "aarch64")]
    let compiler = "/usr/bin/cc";
    assert!(
        Command::new(compiler)
            .args(["-static", "-O2", "-o"])
            .arg(&executable)
            .arg(source)
            .status()
            .unwrap()
            .success()
    );
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o555)).unwrap();
    executable
}

fn fixture() -> (TempDir, i32) {
    let work = TempDir::new().unwrap();
    let pid = 4242;
    let process = work.path().join(pid.to_string());
    std::fs::create_dir_all(process.join("task/4242")).unwrap();
    std::fs::create_dir_all(process.join("fd")).unwrap();
    std::fs::create_dir_all(process.join("root/bin")).unwrap();
    std::fs::write(process.join("task/4242/children"), b"\n").unwrap();
    for descriptor in 0..=2 {
        std::os::unix::fs::symlink("/dev/null", process.join("fd").join(descriptor.to_string())).unwrap();
    }
    let executable = process.join("root/bin/app");
    std::fs::write(&executable, b"elf").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o555)).unwrap();
    std::fs::write(
        process.join("maps"),
        concat!(
            "00400000-00401000 r-xp 00000000 08:01 1 /bin/app\n",
            "00600000-00601000 rw-p 00000000 00:00 0 [heap]\n",
            "7fff0000-7fff1000 r-xp 00000000 00:00 0 [vdso]\n",
        ),
    )
    .unwrap();
    std::fs::write(work.path().join("locks"), b"").unwrap();
    (work, pid)
}

#[test]
fn synthetic_proc_fixture_is_closed_world() {
    let (work, pid) = fixture();
    let process = work.path().join(pid.to_string());
    assert_eq!(classify(work.path(), pid, &[]), 0);

    std::fs::write(process.join("task/4242/children"), b"9\n").unwrap();
    assert_ne!(classify(work.path(), pid, &[]), 0, "child");
    std::fs::write(process.join("task/4242/children"), b"\n").unwrap();

    std::fs::create_dir(process.join("task/4243")).unwrap();
    assert_ne!(classify(work.path(), pid, &[]), 0, "thread");
    std::fs::remove_dir(process.join("task/4243")).unwrap();

    std::os::unix::fs::symlink("socket:[7]", process.join("fd/9")).unwrap();
    assert_ne!(classify(work.path(), pid, &[]), 0, "unknown fd");
    assert_eq!(classify(work.path(), pid, &[9]), 0, "explicit private fd");
    std::fs::remove_file(process.join("fd/9")).unwrap();

    let safe_maps = std::fs::read(process.join("maps")).unwrap();
    for unsafe_map in [
        b"00400000-00401000 r-xp 00000000 08:01 1 /bin/app (deleted)\n".as_slice(),
        b"00400000-00401000 r-xs 00000000 08:01 1 /bin/app\n".as_slice(),
        b"00400000-00401000 rwxp 00000000 00:00 0\n".as_slice(),
        b"00400000-00401000 r--p 00000000 00:00 0 [unknown]\n".as_slice(),
    ] {
        std::fs::write(process.join("maps"), unsafe_map).unwrap();
        assert_ne!(classify(work.path(), pid, &[]), 0, "unsafe mapping");
    }
    std::fs::write(process.join("maps"), safe_maps).unwrap();
    std::fs::write(
        work.path().join("locks"),
        b"1: POSIX ADVISORY WRITE 4242 08:01:1 0 EOF\n",
    )
    .unwrap();
    assert_ne!(classify(work.path(), pid, &[]), 0, "owned lock");
}

#[test]
fn live_process_admits_only_before_unknown_descriptor() {
    let work = TempDir::new().unwrap();
    let executable = live_fixture(work.path());
    let mapped = executable.to_string_lossy();
    let mut child = Command::new(&executable)
        .env_clear()
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = i32::try_from(child.id()).unwrap();
    wait_for_mapping(pid, &mapped);
    let inherited = harness_private_fds(pid);
    assert_eq!(classify(Path::new("/proc"), pid, &inherited), 0, "minimal live child");
    child.kill().unwrap();
    child.wait().unwrap();

    let mut extra = Command::new("/bin/sh")
        .args(["-c", &format!("exec 9</dev/null; exec {}", executable.display())])
        .env_clear()
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = i32::try_from(extra.id()).unwrap();
    wait_for_mapping(pid, &mapped);
    let inherited = harness_private_fds(pid);
    assert_ne!(classify(Path::new("/proc"), pid, &inherited), 0, "unknown live fd");
    let mut recognized = inherited;
    recognized.push(9);
    assert_eq!(
        classify(Path::new("/proc"), pid, &recognized),
        0,
        "recognized private fd"
    );
    extra.kill().unwrap();
    extra.wait().unwrap();
}
