#![cfg(target_os = "linux")]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;

use hl_engine::activation::GuestIsa;
use hl_engine::engine::ExitKind;
use hl_engine::runtime::{Builder, Input, Rootfs};

struct Artifacts {
    engine: PathBuf,
    rootfs: PathBuf,
    workload: PathBuf,
}

impl Artifacts {
    fn from_environment() -> Self {
        let artifacts = Self {
            engine: Self::path("HL_NESTED_X86_ENGINE"),
            rootfs: Self::path("HL_NESTED_X86_ROOTFS"),
            workload: Self::path("HL_NESTED_ARM_WORKLOAD"),
        };
        assert!(artifacts.engine.is_file(), "AMD64 engine must be a file");
        assert!(artifacts.rootfs.is_dir(), "AMD64 rootfs must be a directory");
        assert!(artifacts.workload.is_file(), "ARM64 workload must be a file");
        Self::assert_machine(&artifacts.engine, 62, "AMD64 engine");
        Self::assert_machine(&artifacts.workload, 183, "ARM64 workload");
        assert!(Self::has_loader(&artifacts.rootfs), "AMD64 rootfs lacks its ELF loader");
        artifacts
    }

    fn path(name: &str) -> PathBuf {
        let value =
            std::env::var_os(name).unwrap_or_else(|| panic!("{name} must name a persistent nested-test artifact"));
        let path = PathBuf::from(value);
        assert!(path.is_absolute(), "{name} must be absolute");
        assert!(path.exists(), "{name} does not exist: {}", path.display());
        path
    }

    fn assert_machine(path: &Path, expected: u16, label: &str) {
        let mut header = [0_u8; 20];
        std::fs::File::open(path)
            .and_then(|mut file| file.read_exact(&mut header))
            .unwrap_or_else(|_| panic!("{label} has a truncated ELF header"));
        assert_eq!(&header[..4], b"\x7fELF", "{label} is not ELF");
        assert_eq!(header[4], 2, "{label} must be ELF64");
        assert_eq!(header[5], 1, "{label} must be little-endian");
        assert_eq!(
            u16::from_le_bytes([header[18], header[19]]),
            expected,
            "wrong {label} ISA"
        );
    }

    fn has_loader(root: &Path) -> bool {
        [
            "lib64/ld-linux-x86-64.so.2",
            "lib/x86_64-linux-gnu/ld-linux-x86-64.so.2",
            "lib/x86_64-linux-gnu/ld-2.0.so",
        ]
        .iter()
        .any(|relative| root.join(relative).is_file())
    }
}

#[test]
fn machine_validation() {
    for (machine, label) in [(62_u16, "AMD64"), (183, "ARM64")] {
        let path = std::env::temp_dir().join(format!("hl-nested-elf-{}-{machine}", std::process::id(),));
        let mut header = [0_u8; 20];
        header[..4].copy_from_slice(b"\x7fELF");
        header[4] = 2;
        header[5] = 1;
        header[18..20].copy_from_slice(&machine.to_le_bytes());
        std::fs::write(&path, header).unwrap();
        Artifacts::assert_machine(&path, machine, label);
        std::fs::remove_file(path).unwrap();
    }
}

/// Runs an AMD64 build of this engine as the only outer-engine guest. That
/// guest engine then launches the retained ARM64 workload through its own
/// execution API, proving an actual engine-in-engine path.
#[test]
#[ignore = "requires persistent AMD64 engine/rootfs and ARM64 workload artifacts"]
fn arm_amd_arm() {
    let artifacts = Artifacts::from_environment();
    let rootfs = Rootfs::scratch("hl-engine")
        .with_input(Input::Directory {
            source: Some(artifacts.rootfs),
            relative: PathBuf::new(),
        })
        .with_input(Input::File {
            source: artifacts.workload,
            relative: PathBuf::from("payload/arm-workload"),
            executable: true,
        });
    let build_started = Instant::now();
    let engine = Builder::new(GuestIsa::X86_64, artifacts.engine)
        .with_rootfs(rootfs)
        .with_argument("--guest-isa")
        .with_argument("aarch64")
        .with_argument("/payload/arm-workload")
        .with_argument("--report-exit")
        .build()
        .expect("nested workspace and outer engine must build");
    let build_elapsed = build_started.elapsed();
    let run_started = Instant::now();
    engine.start().expect("AMD64 guest engine must start");
    let exit = engine.wait().expect("nested engine lifecycle must complete");
    let run_elapsed = run_started.elapsed();
    assert_eq!(exit.kind, ExitKind::Code);
    assert_eq!(exit.guest_status, 0);
    assert_eq!(
        engine.destroy().expect("nested engine teardown must succeed"),
        Some(exit)
    );
    eprintln!(
        "nested arm-amd-arm build_ns={} run_ns={}",
        build_elapsed.as_nanos(),
        run_elapsed.as_nanos(),
    );
}
