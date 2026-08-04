use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::sync::atomic::{AtomicU64, Ordering};

use super::source::FileSource;
use super::*;
use crate::composition::{ActivationChannel, CompositionError, RuntimeServices};
use crate::options::Options;
use hl_isa::GuestAddress;
use hl_loader::{ImageRole, ImageSource, ImageSourceError};
use hl_memory::{Backing, MapRequest, Placement, Protection};

static NEXT_FILE: AtomicU64 = AtomicU64::new(1);
const LINK_BASE: u64 = 0x40_0000;
const ENTRY_OFFSET: usize = 0x180;

struct Activation;

impl ActivationChannel for Activation {
    fn send(&self, _: &[u8]) -> Result<(), CompositionError> {
        Ok(())
    }

    fn receive(&self, _: usize) -> Result<Vec<u8>, CompositionError> {
        Ok(Vec::new())
    }
}

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn static_arm() -> Vec<u8> {
    let mut bytes = vec![0_u8; 4096];
    bytes[..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2;
    bytes[5] = 1;
    bytes[6] = 1;
    put_u16(&mut bytes, 16, 2);
    put_u16(&mut bytes, 18, GuestArchitecture::Aarch64.elf_machine());
    put_u32(&mut bytes, 20, 1);
    put_u64(&mut bytes, 24, LINK_BASE + ENTRY_OFFSET as u64);
    put_u64(&mut bytes, 32, 64);
    put_u16(&mut bytes, 52, 64);
    put_u16(&mut bytes, 54, 56);
    put_u16(&mut bytes, 56, 1);
    put_u32(&mut bytes, 64, 1);
    put_u32(&mut bytes, 68, 5);
    put_u64(&mut bytes, 72, 0);
    put_u64(&mut bytes, 80, LINK_BASE);
    put_u64(&mut bytes, 88, LINK_BASE);
    let image_length = bytes.len() as u64;
    put_u64(&mut bytes, 96, image_length);
    put_u64(&mut bytes, 104, image_length);
    put_u64(&mut bytes, 112, 4096);
    for (index, instruction) in [0xd280_0ba8_u32, 0xd280_0540_u32, 0xd400_0001_u32]
        .into_iter()
        .enumerate()
    {
        let offset = ENTRY_OFFSET + index * 4;
        bytes[offset..offset + 4].copy_from_slice(&instruction.to_le_bytes());
    }
    bytes
}

fn routed_arm() -> Vec<u8> {
    let mut bytes = static_arm();
    for (index, instruction) in [0xd280_1588_u32, 0xd400_0001_u32, 0xd280_0ba8_u32, 0xd400_0001_u32]
        .into_iter()
        .enumerate()
    {
        let offset = ENTRY_OFFSET + index * 4;
        bytes[offset..offset + 4].copy_from_slice(&instruction.to_le_bytes());
    }
    bytes
}

fn routed_x86() -> Vec<u8> {
    let mut bytes = vec![0_u8; 4096];
    bytes[..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2;
    bytes[5] = 1;
    bytes[6] = 1;
    put_u16(&mut bytes, 16, 2);
    put_u16(&mut bytes, 18, GuestArchitecture::X86_64.elf_machine());
    put_u32(&mut bytes, 20, 1);
    put_u64(&mut bytes, 24, LINK_BASE + ENTRY_OFFSET as u64);
    put_u64(&mut bytes, 32, 64);
    put_u16(&mut bytes, 52, 64);
    put_u16(&mut bytes, 54, 56);
    put_u16(&mut bytes, 56, 1);
    put_u32(&mut bytes, 64, 1);
    put_u32(&mut bytes, 68, 5);
    put_u64(&mut bytes, 72, 0);
    put_u64(&mut bytes, 80, LINK_BASE);
    put_u64(&mut bytes, 88, LINK_BASE);
    let image_length = bytes.len() as u64;
    put_u64(&mut bytes, 96, image_length);
    put_u64(&mut bytes, 104, image_length);
    put_u64(&mut bytes, 112, 4096);
    bytes[ENTRY_OFFSET..ENTRY_OFFSET + 14]
        .copy_from_slice(&[0xb8, 39, 0, 0, 0, 0x0f, 0x05, 0x89, 0xc7, 0xb8, 60, 0, 0, 0]);
    bytes[ENTRY_OFFSET + 14..ENTRY_OFFSET + 16].copy_from_slice(&[0x0f, 0x05]);
    bytes
}

fn clone_arm() -> Vec<u8> {
    let mut bytes = static_arm();
    for (index, instruction) in [
        0xd281_e000_u32,
        0xf2a0_00a0,
        0xd28a_0001,
        0xd280_0002,
        0xd280_0003,
        0xd280_0004,
        0xd280_1b88,
        0xd400_0001,
        0xb400_0080,
        0xd280_0160,
        0xd280_0ba8,
        0xd400_0001,
        0xd280_02c0,
        0xd280_0bc8,
        0xd400_0001,
    ]
    .into_iter()
    .enumerate()
    {
        let offset = ENTRY_OFFSET + index * 4;
        bytes[offset..offset + 4].copy_from_slice(&instruction.to_le_bytes());
    }
    bytes
}

fn clone_x86() -> Vec<u8> {
    let mut bytes = routed_x86();
    let code = [
        0x48, 0xc7, 0xc7, 0x00, 0x0f, 0x05, 0x00, 0x48, 0xc7, 0xc6, 0x00, 0x50, 0x00, 0x00, 0x31, 0xd2, 0x45, 0x31,
        0xd2, 0x45, 0x31, 0xc0, 0xb8, 56, 0, 0, 0, 0x0f, 0x05, 0x85, 0xc0, 0x74, 0x0c, 0xbf, 11, 0, 0, 0, 0xb8, 60, 0,
        0, 0, 0x0f, 0x05, 0xbf, 22, 0, 0, 0, 0xb8, 231, 0, 0, 0, 0x0f, 0x05,
    ];
    bytes[ENTRY_OFFSET..ENTRY_OFFSET + code.len()].copy_from_slice(&code);
    bytes
}

fn segmented_arm() -> Vec<u8> {
    const TEXT_OFFSET: usize = 4096;
    const TEXT_ADDRESS: u64 = LINK_BASE + 65_536;
    let mut bytes = vec![0_u8; TEXT_OFFSET + 4096];
    bytes[..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2;
    bytes[5] = 1;
    bytes[6] = 1;
    put_u16(&mut bytes, 16, 2);
    put_u16(&mut bytes, 18, GuestArchitecture::Aarch64.elf_machine());
    put_u32(&mut bytes, 20, 1);
    put_u64(&mut bytes, 24, TEXT_ADDRESS);
    put_u64(&mut bytes, 32, 64);
    put_u16(&mut bytes, 52, 64);
    put_u16(&mut bytes, 54, 56);
    put_u16(&mut bytes, 56, 2);
    for (header, flags, offset, address) in [(64, 4, 0_u64, LINK_BASE), (120, 5, TEXT_OFFSET as u64, TEXT_ADDRESS)] {
        put_u32(&mut bytes, header, 1);
        put_u32(&mut bytes, header + 4, flags);
        put_u64(&mut bytes, header + 8, offset);
        put_u64(&mut bytes, header + 16, address);
        put_u64(&mut bytes, header + 24, address);
        put_u64(&mut bytes, header + 32, 4096);
        put_u64(&mut bytes, header + 40, 4096);
        put_u64(&mut bytes, header + 48, 4096);
    }
    for (index, instruction) in [0xd280_0ba8_u32, 0xd280_0aa0_u32, 0xd400_0001_u32]
        .into_iter()
        .enumerate()
    {
        let offset = TEXT_OFFSET + index * 4;
        bytes[offset..offset + 4].copy_from_slice(&instruction.to_le_bytes());
    }
    bytes
}

fn run_arm(image: Vec<u8>, name: &str) -> EngineExit {
    run_image(image, name, GuestIsa::Aarch64)
}

fn run_image(image: Vec<u8>, name: &str, isa: GuestIsa) -> EngineExit {
    run_environment(image, name, isa, Vec::new())
}

fn run_environment(image: Vec<u8>, name: &str, isa: GuestIsa, environment: Vec<Vec<u8>>) -> EngineExit {
    let identity = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("hl-engine-{name}-{}-{identity}", std::process::id()));
    fs::write(&path, image).unwrap();
    let encoded = path.clone().into_os_string().into_vec();
    let plan = RuntimeLaunchPlan {
        rootfs: None,
        executable_host: Some(encoded.clone()),
        arguments: vec![encoded],
        environment,
        result_path: None,
        options: Options::default(),
    };
    let assembly = RuntimeAssembly::new(hl_runtime::RuntimeAssemblyConfig::default()).unwrap();
    let executor = GuestExecutor::default();
    let services = RuntimeServices {
        activation: std::sync::Arc::new(Activation),
        checkpoint_sink: None,
        checkpoint_source: None,
    };
    let result = executor
        .start(isa, &plan, &assembly, &services)
        .and_then(|()| executor.wait(&assembly));
    fs::remove_file(path).unwrap();
    result.unwrap()
}

#[test]
fn environment_stack() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/compat/prebuilt");
    for (isa, folder) in [(GuestIsa::Aarch64, "aarch64"), (GuestIsa::X86_64, "x86_64")] {
        let image = fs::read(root.join(folder).join("environment")).unwrap();
        assert_eq!(
            run_environment(
                image,
                "environment",
                isa,
                vec![b"TZ=UTC\xff".to_vec(), b"EMPTY=".to_vec()],
            ),
            EngineExit {
                kind: ExitKind::Code,
                guest_status: 0,
                detail: 0,
                fault: None,
            },
        );
    }
}

#[test]
fn bootstrap_instructions_execute() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/compat/prebuilt");
    let arm = fs::read(root.join("aarch64/write")).unwrap();
    let x86 = fs::read(root.join("x86_64/exit")).unwrap();
    let x86_write = fs::read(root.join("x86_64/write")).unwrap();
    assert_eq!(
        run_image(arm, "bootstrap-arm-write", GuestIsa::Aarch64),
        EngineExit {
            kind: ExitKind::Code,
            guest_status: 0,
            detail: 0,
            fault: None
        },
    );
    assert_eq!(
        run_image(x86, "bootstrap-x86-exit", GuestIsa::X86_64),
        EngineExit {
            kind: ExitKind::Code,
            guest_status: 42,
            detail: 0,
            fault: None
        },
    );
    assert_eq!(
        run_image(x86_write, "bootstrap-x86-write", GuestIsa::X86_64),
        EngineExit {
            kind: ExitKind::Code,
            guest_status: 0,
            detail: 0,
            fault: None
        },
    );
}

#[test]
fn pthread_clone_executes() {
    assert_eq!(
        run_image(clone_arm(), "clone-arm", GuestIsa::Aarch64),
        EngineExit {
            kind: ExitKind::Code,
            guest_status: 22,
            detail: 0,
            fault: None
        },
    );
    assert_eq!(
        run_image(clone_x86(), "clone-x86", GuestIsa::X86_64),
        EngineExit {
            kind: ExitKind::Code,
            guest_status: 22,
            detail: 0,
            fault: None
        },
    );
}

#[test]
fn clone_teardown() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/compat/prebuilt");
    for (isa, folder) in [(GuestIsa::Aarch64, "aarch64"), (GuestIsa::X86_64, "x86_64")] {
        let image = fs::read(root.join(folder).join("clone")).unwrap();
        assert_eq!(
            run_image(image, "clone-teardown", isa),
            EngineExit {
                kind: ExitKind::Code,
                guest_status: 0,
                detail: 0,
                fault: None,
            },
        );
    }
}

fn source_root(name: &str) -> std::path::PathBuf {
    let identity = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("hl-engine-source-{name}-{}-{identity}", std::process::id()))
}

fn mapped_arena() -> (
    Arc<VirtualMemory>,
    Arc<hl_memory::MappingCoordinator<MappingHostAdapter>>,
) {
    let arena = Arc::new(VirtualMemory::reserve(super::ARENA_LENGTH).unwrap());
    let memory = Arc::new(hl_memory::MappingCoordinator::with_address_space(
        MappingHostAdapter::new(Arc::clone(&arena)),
        hl_memory::AddressSpaceId { slot: 1, generation: 1 },
    ));
    let request = MapRequest {
        placement: Placement::Fixed(GuestAddress::new(0)),
        length: 4096,
        alignment: 4096,
        protection: Protection::READ.union(Protection::WRITE),
        backing: Backing::Anonymous {
            identity: 1,
            shared: false,
        },
        backing_offset: 0,
    };
    memory.map(request).unwrap();
    (arena, memory)
}

fn install_test_ipc(assembly: &RuntimeAssembly) {
    let shared = Arc::new(hl_memory::SharedObjectStore::new(hl_memory::SharedLimits::default()).unwrap());
    assembly.install_ipc(shared).unwrap();
}

#[test]
fn rooted_symlink_resolves() {
    let root = source_root("internal-link");
    fs::create_dir_all(root.join("lib")).unwrap();
    fs::create_dir_all(root.join("real")).unwrap();
    fs::write(root.join("real/ld.so"), b"interpreter").unwrap();
    std::os::unix::fs::symlink("/real/ld.so", root.join("lib/ld.so")).unwrap();
    let root_bytes = root.clone().into_os_string().into_vec();
    let mut source = FileSource::new(Some(&root_bytes));
    assert_eq!(
        source.read_image(ImageRole::Interpreter, b"/lib/ld.so", 64).unwrap(),
        b"interpreter",
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rooted_bytes_preserved() {
    let base = source_root("bytes");
    let root = base.join(OsString::from_vec(vec![b'r', 0xff]));
    fs::create_dir_all(root.join("lib")).unwrap();
    let name = OsString::from_vec(vec![b'l', b'd', 0xfe]);
    fs::write(root.join("lib").join(name), b"raw path").unwrap();
    let root_bytes = root.clone().into_os_string().into_vec();
    let mut source = FileSource::new(Some(&root_bytes));
    assert_eq!(
        source.read_image(ImageRole::Interpreter, b"/lib/ld\xfe", 64).unwrap(),
        b"raw path",
    );
    fs::remove_dir_all(base).unwrap();
}

#[test]
fn rooted_escape_fails() {
    let root = source_root("escape-root");
    let outside = source_root("escape-outside");
    fs::create_dir_all(root.join("lib")).unwrap();
    fs::write(&outside, b"host escape").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("lib/ld.so")).unwrap();
    let root_bytes = root.clone().into_os_string().into_vec();
    let mut source = FileSource::new(Some(&root_bytes));
    assert_eq!(
        source.read_image(ImageRole::Interpreter, b"/lib/ld.so", 64),
        Err(ImageSourceError::NotFound),
    );
    fs::remove_dir_all(root).unwrap();
    fs::remove_file(outside).unwrap();
}

#[test]
fn main_ignores_root() {
    let main = source_root("main");
    fs::write(&main, b"main image").unwrap();
    let main_bytes = main.clone().into_os_string().into_vec();
    let mut source = FileSource::new(Some(b"/missing-root"));
    assert_eq!(
        source.read_image(ImageRole::Main, &main_bytes, 64).unwrap(),
        b"main image",
    );
    fs::remove_file(main).unwrap();
}

#[test]
fn rooted_errors_precede() {
    let root = source_root("errors");
    fs::create_dir_all(root.join("lib")).unwrap();
    fs::write(root.join("lib/large.so"), [0_u8; 8]).unwrap();
    let root_bytes = root.clone().into_os_string().into_vec();
    let mut source = FileSource::new(Some(&root_bytes));
    assert_eq!(
        source.read_image(ImageRole::Interpreter, b"/lib/missing.so", 4),
        Err(ImageSourceError::NotFound),
    );
    assert_eq!(
        source.read_image(ImageRole::Interpreter, b"/lib/large.so", 4),
        Err(ImageSourceError::TooLarge),
    );
    assert_eq!(
        source.read_image(ImageRole::Interpreter, b"/lib/\0ld.so", 64),
        Err(ImageSourceError::AccessDenied),
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn static_arm_loads() {
    assert_eq!(run_arm(static_arm(), "static-arm"), GuestExecutor::code(42));
}

#[test]
fn segmented_arm_loads() {
    assert_eq!(run_arm(segmented_arm(), "segmented-arm"), GuestExecutor::code(85),);
}

#[test]
fn router_resumes_isas() {
    assert_eq!(
        run_image(routed_arm(), "routed-arm", GuestIsa::Aarch64),
        GuestExecutor::code(2),
    );
    assert_eq!(
        run_image(routed_x86(), "routed-x86", GuestIsa::X86_64),
        GuestExecutor::code(2),
    );
}

#[test]
fn ppoll_routes_isas() {
    use hl_runtime::{RuntimeSyscallTrap, RuntimeTrapOutcome};

    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let (arena, mappings) = mapped_arena();
        let mut pollfd = [0_u8; 8];
        pollfd[..4].copy_from_slice(&9_i32.to_le_bytes());
        pollfd[4..6].copy_from_slice(&1_i16.to_le_bytes());
        arena.write(0, &pollfd).unwrap();
        let assembly = RuntimeAssembly::new(Default::default()).unwrap();
        let router = GuestExecutor::router(
            Arc::clone(&arena),
            mappings,
            &RuntimeLaunchPlan {
                rootfs: None,
                executable_host: None,
                arguments: Vec::new(),
                environment: Vec::new(),
                result_path: None,
                options: Options::default(),
            },
            &assembly,
            architecture,
            Arc::new(super::readiness::Cancellation::new().unwrap()),
        )
        .unwrap();
        let mut cpu = match architecture {
            GuestArchitecture::Aarch64 => {
                let mut cpu = Aarch64CpuState::default();
                cpu.registers[1] = 1;
                cpu.registers[8] = 73;
                ExecutionCpuSnapshot::Aarch64(cpu)
            }
            GuestArchitecture::X86_64 => {
                let mut cpu = CpuState::default();
                cpu.registers[0] = 271;
                cpu.registers[6] = 1;
                ExecutionCpuSnapshot::X86_64(cpu)
            }
        };
        assert_eq!(router.dispatch(architecture, &mut cpu), RuntimeTrapOutcome::Continue,);
        let result = match cpu {
            ExecutionCpuSnapshot::Aarch64(cpu) => cpu.registers[0],
            ExecutionCpuSnapshot::X86_64(cpu) => cpu.registers[0],
        };
        assert_eq!(result, 1);
        arena.read(0, &mut pollfd).unwrap();
        assert_eq!(i16::from_le_bytes(pollfd[6..].try_into().unwrap()), 0x20);
    }
}

fn route_call(router: &RuntimeSyscallRouter, architecture: GuestArchitecture, number: u64, arguments: [u64; 6]) -> u64 {
    use hl_runtime::{RuntimeSyscallTrap, RuntimeTrapOutcome};

    let mut cpu = match architecture {
        GuestArchitecture::Aarch64 => {
            let mut cpu = Aarch64CpuState::default();
            cpu.registers[..6].copy_from_slice(&arguments);
            cpu.registers[8] = number;
            ExecutionCpuSnapshot::Aarch64(cpu)
        }
        GuestArchitecture::X86_64 => {
            let mut cpu = CpuState::default();
            for (register, argument) in [7, 6, 2, 10, 8, 9].into_iter().zip(arguments) {
                cpu.registers[register] = argument;
            }
            cpu.registers[0] = number;
            ExecutionCpuSnapshot::X86_64(cpu)
        }
    };
    assert_eq!(router.dispatch(architecture, &mut cpu), RuntimeTrapOutcome::Continue,);
    match cpu {
        ExecutionCpuSnapshot::Aarch64(cpu) => cpu.registers[0],
        ExecutionCpuSnapshot::X86_64(cpu) => cpu.registers[0],
    }
}

fn route_exit(
    router: &RuntimeSyscallRouter,
    architecture: GuestArchitecture,
    number: u64,
    arguments: [u64; 6],
) -> hl_runtime::RuntimeTrapOutcome {
    use hl_runtime::RuntimeSyscallTrap;
    let mut cpu = match architecture {
        GuestArchitecture::Aarch64 => {
            let mut cpu = Aarch64CpuState::default();
            cpu.registers[..6].copy_from_slice(&arguments);
            cpu.registers[8] = number;
            ExecutionCpuSnapshot::Aarch64(cpu)
        }
        GuestArchitecture::X86_64 => {
            let mut cpu = CpuState::default();
            for (register, argument) in [7, 6, 2, 10, 8, 9].into_iter().zip(arguments) {
                cpu.registers[register] = argument;
            }
            cpu.registers[0] = number;
            ExecutionCpuSnapshot::X86_64(cpu)
        }
    };
    router.dispatch(architecture, &mut cpu)
}

#[test]
fn pidfd_fstat() {
    use hl_linux::{Errno, LinuxResult};

    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let assembly = RuntimeAssembly::new(Default::default()).unwrap();
        let (arena, mappings) = mapped_arena();
        let router = GuestExecutor::router(
            Arc::clone(&arena),
            mappings,
            &RuntimeLaunchPlan {
                rootfs: None,
                executable_host: None,
                arguments: Vec::new(),
                environment: Vec::new(),
                result_path: None,
                options: Options::default(),
            },
            &assembly,
            architecture,
            Arc::new(super::readiness::Cancellation::new().unwrap()),
        )
        .unwrap();
        let snapshot = assembly.tasks().snapshot();
        let process = snapshot
            .processes
            .iter()
            .find(|process| Some(process.id) != snapshot.init)
            .expect("router process exists")
            .id;
        let (fstat, duplicate, close) = match architecture {
            GuestArchitecture::Aarch64 => (80, 23, 57),
            GuestArchitecture::X86_64 => (5, 32, 3),
        };
        let descriptor = route_call(&router, architecture, 434, [u64::from(process.number()), 0, 0, 0, 0, 0]);
        assert!(descriptor <= i32::MAX as u64);
        assert_eq!(route_call(&router, architecture, fstat, [descriptor, 0, 0, 0, 0, 0]), 0);
        let alias = route_call(&router, architecture, duplicate, [descriptor, 0, 0, 0, 0, 0]);
        assert_eq!(route_call(&router, architecture, fstat, [alias, 256, 0, 0, 0, 0]), 0);
        let size = architecture.linux_stat_size();
        let mut first = vec![0; size];
        let mut second = vec![0; size];
        arena.read(0, &mut first).unwrap();
        arena.read(256, &mut second).unwrap();
        assert_eq!(first, second);
        let mode_offset = match architecture {
            GuestArchitecture::Aarch64 => 16,
            GuestArchitecture::X86_64 => 24,
        };
        assert_eq!(
            u32::from_le_bytes(first[mode_offset..mode_offset + 4].try_into().unwrap()),
            0o100600
        );
        assert_eq!(route_call(&router, architecture, close, [descriptor, 0, 0, 0, 0, 0]), 0);
        assert_eq!(route_call(&router, architecture, close, [alias, 0, 0, 0, 0, 0]), 0);
        assert_eq!(
            route_call(&router, architecture, fstat, [alias, 4096, 0, 0, 0, 0]),
            LinuxResult::Error(Errno::EBADF).encode(),
        );

        let live = route_call(&router, architecture, 434, [u64::from(process.number()), 0, 0, 0, 0, 0]);
        assert_eq!(
            route_call(&router, architecture, fstat, [live, 4096, 0, 0, 0, 0]),
            LinuxResult::Error(Errno::EFAULT).encode(),
        );
    }
}

#[test]
fn production_thread_registration() {
    use hl_linux::{Errno, LinuxResult};
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let assembly = RuntimeAssembly::new(Default::default()).unwrap();
        let (arena, mappings) = mapped_arena();
        let router = GuestExecutor::router(
            Arc::clone(&arena),
            mappings,
            &RuntimeLaunchPlan {
                rootfs: None,
                executable_host: None,
                arguments: Vec::new(),
                environment: Vec::new(),
                result_path: None,
                options: Options::default(),
            },
            &assembly,
            architecture,
            Arc::new(super::readiness::Cancellation::new().unwrap()),
        )
        .unwrap();
        let (set_tid, set_robust, get_robust, gettid, exit) = match architecture {
            GuestArchitecture::Aarch64 => (96, 99, 100, 178, 93),
            GuestArchitecture::X86_64 => (218, 273, 274, 186, 60),
        };
        arena.write(64, &7_u32.to_le_bytes()).unwrap();
        let tid = route_call(&router, architecture, gettid, [0; 6]);
        assert_ne!(tid, 0);
        assert_eq!(route_call(&router, architecture, set_tid, [64, 0, 0, 0, 0, 0]), tid);

        let mut head = [0_u8; 24];
        head[..8].copy_from_slice(&160_u64.to_le_bytes());
        head[8..16].copy_from_slice(&8_u64.to_le_bytes());
        arena.write(128, &head).unwrap();
        arena.write(160, &128_u64.to_le_bytes()).unwrap();
        arena.write(168, &(0x8000_0000_u32 | tid as u32).to_le_bytes()).unwrap();
        assert_eq!(
            route_call(&router, architecture, set_robust, [128, 23, 0, 0, 0, 0]),
            LinuxResult::Error(Errno::EINVAL).encode(),
        );
        assert_eq!(route_call(&router, architecture, set_robust, [128, 24, 0, 0, 0, 0],), 0);
        assert_eq!(
            route_call(&router, architecture, get_robust, [0, 256, 264, 0, 0, 0],),
            0
        );
        let mut registration = [0; 16];
        arena.read(256, &mut registration).unwrap();
        assert_eq!(u64::from_le_bytes(registration[..8].try_into().unwrap()), 128);
        assert_eq!(u64::from_le_bytes(registration[8..].try_into().unwrap()), 24);
        assert_eq!(
            route_call(&router, architecture, get_robust, [tid, 256, 264, 0, 0, 0],),
            0
        );

        arena.write(280, &[0xaa; 16]).unwrap();
        assert_eq!(
            route_call(&router, architecture, get_robust, [0, 280, 4094, 0, 0, 0],),
            LinuxResult::Error(Errno::EFAULT).encode()
        );
        let mut unchanged = [0; 16];
        arena.read(280, &mut unchanged).unwrap();
        assert_eq!(unchanged, [0xaa; 16]);
        assert_eq!(
            route_call(
                &router,
                architecture,
                get_robust,
                [u64::from(u32::MAX), 4094, 4094, 0, 0, 0],
            ),
            LinuxResult::Error(Errno::ESRCH).encode()
        );
        let foreign = assembly
            .tasks()
            .snapshot()
            .threads
            .into_iter()
            .find(|thread| thread.id.number() != tid as u32)
            .unwrap()
            .id
            .number();
        assert_eq!(
            route_call(
                &router,
                architecture,
                get_robust,
                [u64::from(foreign), 4094, 4094, 0, 0, 0],
            ),
            LinuxResult::Error(Errno::EPERM).encode()
        );
        assert_eq!(
            route_exit(&router, architecture, exit, [0, 0, 0, 0, 0, 0]),
            hl_runtime::RuntimeTrapOutcome::Exit(0),
        );
        let mut cleared = [0xff; 4];
        arena.read(64, &mut cleared).unwrap();
        assert_eq!(cleared, [0; 4]);
        let mut owner = [0; 4];
        arena.read(168, &mut owner).unwrap();
        assert_eq!(u32::from_le_bytes(owner), 0xc000_0000);
    }
}

#[test]
fn production_memory_family() {
    use hl_linux::{Errno, LinuxResult};
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let arena = Arc::new(VirtualMemory::reserve(ARENA_LENGTH).unwrap());
        let mappings = Arc::new(hl_memory::MappingCoordinator::with_address_space(
            MappingHostAdapter::new(Arc::clone(&arena)),
            hl_memory::AddressSpaceId { slot: 1, generation: 1 },
        ));
        let assembly = RuntimeAssembly::new(Default::default()).unwrap();
        let router = GuestExecutor::router(
            Arc::clone(&arena),
            mappings,
            &RuntimeLaunchPlan {
                rootfs: None,
                executable_host: None,
                arguments: Vec::new(),
                environment: Vec::new(),
                result_path: None,
                options: Options::default(),
            },
            &assembly,
            architecture,
            Arc::new(super::readiness::Cancellation::new().unwrap()),
        )
        .unwrap();
        let (brk, mmap, mprotect, munmap) = match architecture {
            GuestArchitecture::Aarch64 => (214, 222, 226, 215),
            GuestArchitecture::X86_64 => (12, 9, 10, 11),
        };
        assert_eq!(route_call(&router, architecture, brk, [0; 6]), 0x80_0000);
        assert_eq!(
            route_call(&router, architecture, brk, [0x80_1001, 0, 0, 0, 0, 0]),
            0x80_1001
        );
        assert_eq!(
            route_call(&router, architecture, brk, [0x80_0000, 0, 0, 0, 0, 0]),
            0x80_0000
        );
        let address = route_call(&router, architecture, mmap, [0, 4096, 3, 0x22, u64::MAX, 0]);
        assert!(address < ARENA_LENGTH as u64);
        arena.write(address, b"memory").unwrap();
        assert_eq!(
            route_call(&router, architecture, mprotect, [address, 4096, 1, 0, 0, 0]),
            0
        );
        assert!(arena.write(address, b"blocked").is_err());
        assert_eq!(
            route_call(&router, architecture, mmap, [address, 4096, 1, 0x10_0022, u64::MAX, 0]),
            LinuxResult::Error(Errno::EEXIST).encode()
        );
        let mut retained = [0; 6];
        arena.read(address, &mut retained).unwrap();
        assert_eq!(&retained, b"memory");
        assert_eq!(
            route_call(&router, architecture, mprotect, [address, 4096, 6, 0, 0, 0]),
            LinuxResult::Error(Errno::EINVAL).encode()
        );
        assert_eq!(
            route_call(&router, architecture, munmap, [address, 4096, 0, 0, 0, 0]),
            0
        );
        assert!(arena.read(address, &mut [0]).is_err());
    }
}

#[test]
fn shared_thread_routes() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let arena = Arc::new(VirtualMemory::reserve(ARENA_LENGTH).unwrap());
        let shared = Arc::new(hl_memory::SharedObjectStore::new(hl_memory::SharedLimits::default()).unwrap());
        let mappings = Arc::new(hl_memory::MappingCoordinator::with_shared_space(
            MappingHostAdapter::new(Arc::clone(&arena)),
            shared,
            hl_memory::AddressSpaceId { slot: 1, generation: 1 },
        ));
        mappings
            .map(MapRequest {
                placement: Placement::Fixed(GuestAddress::new(0)),
                length: 4096,
                alignment: 4096,
                protection: Protection::READ.union(Protection::WRITE),
                backing: Backing::Anonymous {
                    identity: 1,
                    shared: false,
                },
                backing_offset: 0,
            })
            .unwrap();
        arena.write(64, b"threads\0").unwrap();
        let assembly = RuntimeAssembly::new(Default::default()).unwrap();
        install_test_ipc(&assembly);
        let first_cancel = Arc::new(super::readiness::Cancellation::new().unwrap());
        let route = super::routing::create(
            Arc::clone(&arena),
            mappings,
            &RuntimeLaunchPlan {
                rootfs: None,
                executable_host: None,
                arguments: Vec::new(),
                environment: Vec::new(),
                result_path: Some(b"thread-trace".to_vec()),
                options: Options::default(),
            },
            &assembly,
            architecture,
            Arc::clone(&first_cancel),
            None,
            Arc::new(super::image_data::Entropy),
        )
        .unwrap();
        let tasks = assembly.tasks();
        let second = tasks
            .commit_clone_thread(tasks.begin_clone_thread(route.thread).unwrap())
            .unwrap();
        let second_cancel = Arc::new(super::readiness::Cancellation::new().unwrap());
        let second_router = route.process.router(second, Arc::clone(&second_cancel), None);
        let (brk, gettid, duplicate, memfd, seek) = match architecture {
            GuestArchitecture::Aarch64 => (214, 178, 23, 279, 62),
            GuestArchitecture::X86_64 => (12, 186, 32, 319, 8),
        };
        assert_eq!(
            route_call(&route.router, architecture, brk, [0x80_2000, 0, 0, 0, 0, 0],),
            0x80_2000
        );
        assert_eq!(route_call(&second_router, architecture, brk, [0; 6]), 0x80_2000);
        assert_ne!(
            route_call(&route.router, architecture, gettid, [0; 6]),
            route_call(&second_router, architecture, gettid, [0; 6]),
        );
        assert_eq!(route_call(&route.router, architecture, duplicate, [0; 6]), 3);
        assert_eq!(
            assembly.descriptors().descriptor_table().flags(3).unwrap(),
            hl_descriptor::DescriptorFlags::from_bits(0)
        );
        assert_eq!(route_call(&route.router, architecture, memfd, [64, 0, 0, 0, 0, 0]), 4);
        assert_eq!(route_call(&second_router, architecture, seek, [4, 0, 0, 0, 0, 0]), 0);
        let first_trace = route.router.trace().unwrap();
        let second_trace = second_router.trace().unwrap();
        assert_eq!(first_trace.last().unwrap().name, "memfd_create");
        assert_eq!(second_trace.last().unwrap().name, "lseek");
        if architecture == GuestArchitecture::X86_64 {
            use hl_runtime::RuntimeSyscallTrap;
            let mut first_cpu = CpuState::default();
            first_cpu.registers[0] = 158;
            first_cpu.registers[7] = 0x1002;
            first_cpu.registers[6] = 0x1111;
            let mut first_cpu = ExecutionCpuSnapshot::X86_64(first_cpu);
            route.router.dispatch(architecture, &mut first_cpu);
            let mut second_cpu = CpuState::default();
            second_cpu.registers[0] = 158;
            second_cpu.registers[7] = 0x1002;
            second_cpu.registers[6] = 0x2222;
            let mut second_cpu = ExecutionCpuSnapshot::X86_64(second_cpu);
            second_router.dispatch(architecture, &mut second_cpu);
            let ExecutionCpuSnapshot::X86_64(first_cpu) = first_cpu else {
                unreachable!()
            };
            let ExecutionCpuSnapshot::X86_64(second_cpu) = second_cpu else {
                unreachable!()
            };
            assert_eq!((first_cpu.fs_base, second_cpu.fs_base), (0x1111, 0x2222));
        }
        first_cancel.request(9);
        assert_eq!(first_cancel.signal(), Some(9));
        assert_eq!(second_cancel.signal(), None);
    }
}

#[test]
fn production_memfd_family() {
    use hl_linux::{Errno, LinuxResult};
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let arena = Arc::new(VirtualMemory::reserve(ARENA_LENGTH).unwrap());
        let shared = Arc::new(
            hl_memory::SharedObjectStore::new(hl_memory::SharedLimits {
                objects: 4,
                object_bytes: 4096,
                total_bytes: 8192,
            })
            .unwrap(),
        );
        let mappings = Arc::new(hl_memory::MappingCoordinator::with_shared_space(
            MappingHostAdapter::new(Arc::clone(&arena)),
            Arc::clone(&shared),
            hl_memory::AddressSpaceId { slot: 1, generation: 1 },
        ));
        mappings
            .map(MapRequest {
                placement: Placement::Fixed(GuestAddress::new(0)),
                length: 4096,
                alignment: 4096,
                protection: Protection::READ.union(Protection::WRITE),
                backing: Backing::Anonymous {
                    identity: 1,
                    shared: false,
                },
                backing_offset: 0,
            })
            .unwrap();
        arena.write(64, b"engine-memfd\0").unwrap();
        arena.write(128, b"shared-bytes").unwrap();
        arena.write(512, &[b'n'; 250]).unwrap();
        let assembly = RuntimeAssembly::new(hl_runtime::RuntimeAssemblyConfig {
            descriptor_limit: 5,
            ..Default::default()
        })
        .unwrap();
        let router = GuestExecutor::router(
            Arc::clone(&arena),
            mappings,
            &RuntimeLaunchPlan {
                rootfs: None,
                executable_host: None,
                arguments: Vec::new(),
                environment: Vec::new(),
                result_path: None,
                options: Options::default(),
            },
            &assembly,
            architecture,
            Arc::new(super::readiness::Cancellation::new().unwrap()),
        )
        .unwrap();
        let (memfd, write, read, seek, duplicate, close) = match architecture {
            GuestArchitecture::Aarch64 => (279, 64, 63, 62, 23, 57),
            GuestArchitecture::X86_64 => (319, 1, 0, 8, 32, 3),
        };
        assert_eq!(route_call(&router, architecture, memfd, [64, 1, 0, 0, 0, 0]), 3);
        assert!(
            assembly
                .descriptors()
                .descriptor_table()
                .flags(3)
                .unwrap()
                .closes_on_exec()
        );
        assert_eq!(route_call(&router, architecture, write, [3, 128, 12, 0, 0, 0]), 12);
        assert_eq!(route_call(&router, architecture, duplicate, [3, 0, 0, 0, 0, 0]), 4);
        assert!(
            !assembly
                .descriptors()
                .descriptor_table()
                .flags(4)
                .unwrap()
                .closes_on_exec()
        );
        assert_eq!(route_call(&router, architecture, seek, [4, 0, 0, 0, 0, 0]), 0);
        assert_eq!(route_call(&router, architecture, read, [3, 256, 12, 0, 0, 0]), 12);
        let mut output = [0; 12];
        arena.read(256, &mut output).unwrap();
        assert_eq!(&output, b"shared-bytes");
        assert_eq!(
            route_call(&router, architecture, memfd, [64, 0, 0, 0, 0, 0]),
            LinuxResult::Error(Errno::EMFILE).encode()
        );
        assert_eq!(shared.snapshot().objects.len(), 1);
        assert_eq!(
            route_call(&router, architecture, memfd, [0, 0, 0, 0, 0, 0]),
            LinuxResult::Error(Errno::EFAULT).encode()
        );
        assert_eq!(
            route_call(&router, architecture, memfd, [512, 0, 0, 0, 0, 0]),
            LinuxResult::Error(Errno::EINVAL).encode()
        );
        assert_eq!(
            route_call(&router, architecture, memfd, [64, 0x20, 0, 0, 0, 0]),
            LinuxResult::Error(Errno::EINVAL).encode()
        );
        assert_eq!(
            route_call(&router, architecture, memfd, [64, 4, 0, 0, 0, 0]),
            LinuxResult::Error(Errno::ENOSYS).encode()
        );
        assert_eq!(route_call(&router, architecture, close, [3, 0, 0, 0, 0, 0]), 0);
        assert_eq!(route_call(&router, architecture, close, [4, 0, 0, 0, 0, 0]), 0);
        assert_eq!(route_call(&router, architecture, memfd, [64, 0, 0, 0, 0, 0]), 3);
        assert_eq!(shared.snapshot().objects.len(), 1);
    }
}

#[test]
fn descriptor_routes_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let assembly = RuntimeAssembly::new(Default::default()).unwrap();
        let (arena, mappings) = mapped_arena();
        let router = GuestExecutor::router(
            arena,
            mappings,
            &RuntimeLaunchPlan {
                rootfs: None,
                executable_host: None,
                arguments: Vec::new(),
                environment: Vec::new(),
                result_path: None,
                options: Options::default(),
            },
            &assembly,
            architecture,
            Arc::new(super::readiness::Cancellation::new().unwrap()),
        )
        .unwrap();
        let (dup, fcntl, dup3) = match architecture {
            GuestArchitecture::Aarch64 => (23, 25, 24),
            GuestArchitecture::X86_64 => (32, 72, 292),
        };
        assert_eq!(route_call(&router, architecture, dup, [0; 6]), 3);
        assert_eq!(route_call(&router, architecture, fcntl, [3, 2, 1, 0, 0, 0]), 0,);
        assert_eq!(route_call(&router, architecture, fcntl, [3, 1, 0, 0, 0, 0]), 1,);
        assert_eq!(route_call(&router, architecture, dup3, [3, 7, 0o2000000, 0, 0, 0],), 7,);
    }
}

#[test]
fn event_routes_isas() {
    use hl_linux::{Errno, LinuxResult};

    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let assembly = RuntimeAssembly::new(Default::default()).unwrap();
        let (arena, mappings) = mapped_arena();
        let router = GuestExecutor::router(
            Arc::clone(&arena),
            mappings,
            &RuntimeLaunchPlan {
                rootfs: None,
                executable_host: None,
                arguments: Vec::new(),
                environment: Vec::new(),
                result_path: None,
                options: Options::default(),
            },
            &assembly,
            architecture,
            Arc::new(super::readiness::Cancellation::new().unwrap()),
        )
        .unwrap();
        let (read, write, duplicate, close, eventfd, timer_create, timer_set, timer_get, ppoll) = match architecture {
            GuestArchitecture::Aarch64 => (63, 64, 23, 57, 19, 85, 86, 87, 73),
            GuestArchitecture::X86_64 => (0, 1, 32, 3, 290, 283, 286, 287, 271),
        };
        let flags = 0x800 | 0x8_0000;
        assert_eq!(route_call(&router, architecture, eventfd, [0, flags, 0, 0, 0, 0]), 3);
        assert!(
            assembly
                .descriptors()
                .descriptor_table()
                .flags(3)
                .unwrap()
                .closes_on_exec()
        );
        assert_eq!(
            route_call(&router, architecture, read, [3, 64, 8, 0, 0, 0]),
            LinuxResult::Error(Errno::EAGAIN).encode(),
        );
        arena.write(64, &5_u64.to_ne_bytes()).unwrap();
        assert_eq!(route_call(&router, architecture, write, [3, 64, 8, 0, 0, 0]), 8);
        assert_eq!(route_call(&router, architecture, duplicate, [3, 0, 0, 0, 0, 0]), 4);
        assert_eq!(route_call(&router, architecture, read, [4, 72, 8, 0, 0, 0]), 8);
        let mut value = [0; 8];
        arena.read(72, &mut value).unwrap();
        assert_eq!(u64::from_ne_bytes(value), 5);
        assert_eq!(route_call(&router, architecture, close, [3, 0, 0, 0, 0, 0]), 0);
        assert_eq!(route_call(&router, architecture, close, [4, 0, 0, 0, 0, 0]), 0);

        assert_eq!(
            route_call(&router, architecture, timer_create, [1, flags, 0, 0, 0, 0]),
            3
        );
        let mut setting = [0_u8; 32];
        setting[..8].copy_from_slice(&1_i64.to_ne_bytes());
        setting[24..32].copy_from_slice(&20_000_000_i64.to_ne_bytes());
        arena.write(128, &setting).unwrap();
        assert_eq!(route_call(&router, architecture, timer_set, [3, 0, 128, 0, 0, 0]), 0);
        assert_eq!(route_call(&router, architecture, timer_get, [3, 192, 0, 0, 0, 0]), 0);
        let mut current = [0; 32];
        arena.read(192, &mut current).unwrap();
        assert_eq!(i64::from_ne_bytes(current[..8].try_into().unwrap()), 1);
        let mut pollfd = [0_u8; 8];
        pollfd[..4].copy_from_slice(&3_i32.to_ne_bytes());
        pollfd[4..6].copy_from_slice(&1_i16.to_ne_bytes());
        arena.write(256, &pollfd).unwrap();
        assert_eq!(route_call(&router, architecture, ppoll, [256, 1, 0, 0, 0, 0]), 1);
        assert_eq!(route_call(&router, architecture, read, [3, 288, 8, 0, 0, 0]), 8);
        assert_eq!(route_call(&router, architecture, close, [3, 0, 0, 0, 0, 0]), 0);
    }
}

#[test]
fn pipe_routes_isas() {
    use hl_linux::{Errno, LinuxResult};

    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let assembly = RuntimeAssembly::new(Default::default()).unwrap();
        let (arena, mappings) = mapped_arena();
        let router = GuestExecutor::router(
            Arc::clone(&arena),
            mappings,
            &RuntimeLaunchPlan {
                rootfs: None,
                executable_host: None,
                arguments: Vec::new(),
                environment: Vec::new(),
                result_path: None,
                options: Options::default(),
            },
            &assembly,
            architecture,
            Arc::new(super::readiness::Cancellation::new().unwrap()),
        )
        .unwrap();
        let (pipe2, read, write, close, duplicate) = match architecture {
            GuestArchitecture::Aarch64 => (59, 63, 64, 57, 23),
            GuestArchitecture::X86_64 => (293, 0, 1, 3, 32),
        };
        let flags = 0o02004000;
        assert_eq!(route_call(&router, architecture, pipe2, [64, flags, 0, 0, 0, 0]), 0);
        let mut numbers = [0; 8];
        arena.read(64, &mut numbers).unwrap();
        let reader = u32::from_ne_bytes(numbers[..4].try_into().unwrap()) as u64;
        let writer = u32::from_ne_bytes(numbers[4..].try_into().unwrap()) as u64;
        let writer_alias = route_call(&router, architecture, duplicate, [writer, 0, 0, 0, 0, 0]);
        assert_eq!(route_call(&router, architecture, close, [writer, 0, 0, 0, 0, 0]), 0);
        assert!(
            assembly
                .descriptors()
                .descriptor_table()
                .flags(reader as i32)
                .unwrap()
                .closes_on_exec()
        );
        assert_eq!(
            route_call(&router, architecture, read, [reader, 96, 1, 0, 0, 0]),
            LinuxResult::Error(Errno::EAGAIN).encode(),
        );
        arena.write(96, b"p").unwrap();
        assert_eq!(
            route_call(&router, architecture, write, [writer_alias, 96, 1, 0, 0, 0]),
            1
        );
        assert_eq!(route_call(&router, architecture, read, [reader, 104, 1, 0, 0, 0]), 1);
        assert_eq!(route_call(&router, architecture, close, [reader, 0, 0, 0, 0, 0]), 0);
        assert_eq!(
            route_call(&router, architecture, write, [writer_alias, 96, 1, 0, 0, 0]),
            LinuxResult::Error(Errno::EPIPE).encode(),
        );
        assert_eq!(
            route_call(&router, architecture, write, [writer_alias, 96, 1, 0, 0, 0]),
            LinuxResult::Error(Errno::EPIPE).encode(),
        );
        assert_eq!(
            route_call(&router, architecture, close, [writer_alias, 0, 0, 0, 0, 0]),
            0
        );
    }
}

#[test]
fn x86_dup2_routes() {
    let assembly = RuntimeAssembly::new(Default::default()).unwrap();
    let (arena, mappings) = mapped_arena();
    let router = GuestExecutor::router(
        arena,
        mappings,
        &RuntimeLaunchPlan {
            rootfs: None,
            executable_host: None,
            arguments: Vec::new(),
            environment: Vec::new(),
            result_path: None,
            options: Options::default(),
        },
        &assembly,
        GuestArchitecture::X86_64,
        Arc::new(super::readiness::Cancellation::new().unwrap()),
    )
    .unwrap();
    assert_eq!(
        route_call(&router, GuestArchitecture::X86_64, 33, [0, 8, 0, 0, 0, 0],),
        8,
    );
}

#[test]
fn exit_group_cleanup() {
    let assembly = RuntimeAssembly::new(Default::default()).unwrap();
    let (arena, mappings) = mapped_arena();
    let router = GuestExecutor::router(
        Arc::clone(&arena),
        Arc::clone(&mappings),
        &RuntimeLaunchPlan {
            rootfs: None,
            executable_host: None,
            arguments: Vec::new(),
            environment: Vec::new(),
            result_path: None,
            options: Options::default(),
        },
        &assembly,
        GuestArchitecture::X86_64,
        Arc::new(super::readiness::Cancellation::new().unwrap()),
    )
    .unwrap();
    let before = assembly.tasks().snapshot();
    let child = before
        .processes
        .iter()
        .find(|process| Some(process.id) != before.init)
        .expect("router owns a real child")
        .id;

    use hl_runtime::{RuntimeSyscallTrap, RuntimeTrapOutcome};
    let mut cpu = CpuState::default();
    cpu.registers[0] = 231;
    cpu.registers[7] = 37;
    let mut cpu = ExecutionCpuSnapshot::X86_64(cpu);
    assert_eq!(
        router.dispatch(GuestArchitecture::X86_64, &mut cpu),
        RuntimeTrapOutcome::Exit(37),
    );
    assert_eq!(router.take_terminal(), Some(hl_runtime::RuntimeTerminal::Group(37)),);
    let exited = assembly
        .tasks()
        .snapshot()
        .processes
        .into_iter()
        .find(|process| process.id == child)
        .expect("exited child remains waitable");
    assert_eq!(exited.lifecycle, hl_task::ProcessLifecycle::Zombie);
    assert_eq!(exited.exit_status, Some(hl_task::ExitStatus::Code(37)));
    assert!(mappings.ledger().regions().is_empty());
    assert!(arena.read(0, &mut [0; 1]).is_err());
    assert_eq!(
        mappings.map(MapRequest {
            placement: Placement::Fixed(GuestAddress::new(0)),
            length: 4096,
            alignment: 4096,
            protection: Protection::READ,
            backing: Backing::Anonymous {
                identity: 2,
                shared: false,
            },
            backing_offset: 0,
        }),
        Err(hl_memory::MemoryError::NoAddressSpace),
    );
}

#[test]
fn stop_interrupts_running() {
    let executor = GuestExecutor::default();
    let assembly = RuntimeAssembly::new(hl_runtime::RuntimeAssemblyConfig::default()).unwrap();
    install_test_ipc(&assembly);
    let key = &assembly as *const RuntimeAssembly as usize;
    let cancellation = Arc::new(super::readiness::Cancellation::new().unwrap());
    let (arena, mappings) = mapped_arena();
    let routed = super::routing::create(
        arena,
        mappings,
        &RuntimeLaunchPlan {
            rootfs: None,
            executable_host: None,
            arguments: Vec::new(),
            environment: Vec::new(),
            result_path: None,
            options: Options::default(),
        },
        &assembly,
        GuestArchitecture::Aarch64,
        Arc::clone(&cancellation),
        None,
        Arc::new(super::image_data::Entropy),
    )
    .unwrap();
    let threads = Arc::new(super::threads::ThreadSet::new(1).unwrap());
    let space = routed.process.space();
    threads
        .prepare(
            routed.thread,
            routed.process.process_id(),
            Arc::new(routed.router),
            Arc::clone(&cancellation),
            space,
        )
        .unwrap();
    threads
        .stage(
            routed.thread,
            ExecutionSnapshot {
                version: EXECUTION_SNAPSHOT_VERSION,
                cpu: ExecutionCpuSnapshot::Aarch64(Aarch64CpuState::default()),
                cache_epoch: 1,
                fault: None,
            },
        )
        .unwrap()
        .publish();
    executor.state.lock().unwrap().running.insert(key, threads);
    executor.stop(&assembly, StopRequest::Signal(15)).unwrap();
    assert_eq!(cancellation.signal(), Some(15));
}

#[test]
fn stop_before_start() {
    let executor = GuestExecutor::default();
    let assembly = RuntimeAssembly::new(Default::default()).unwrap();
    executor.stop(&assembly, StopRequest::Force).unwrap();
    let exit = executor.wait(&assembly).unwrap();
    assert_eq!(exit.kind, ExitKind::Signal);
    assert_eq!(exit.guest_status, 9);
}

#[test]
fn unsupported_inventory_exact() {
    use hl_linux::{CANONICAL_SYSCALLS, LinuxResult, X86_LEGACY_SYSCALLS};
    use hl_runtime::{RuntimeSyscallTrap, RuntimeTrapOutcome};

    let supported = GuestExecutor::supported_syscalls();
    assert!(supported.contains(&"alarm"), "x86 legacy alarm must reach the task/time runtime");
    assert!(supported.contains(&"time"), "x86 legacy time must reach the task/time runtime");
    assert!(supported.windows(2).all(|pair| pair[0] < pair[1]));
    for name in supported {
        assert!(
            *name == "arch_prctl"
                || CANONICAL_SYSCALLS
                    .iter()
                    .any(|definition| definition.operation.name == *name)
                || X86_LEGACY_SYSCALLS
                    .iter()
                    .any(|definition| definition.operation.name == *name),
            "supported syscall is absent from Linux tables: {name}",
        );
    }

    let arena = Arc::new(VirtualMemory::reserve(ARENA_LENGTH).unwrap());
    let shared = Arc::new(hl_memory::SharedObjectStore::new(hl_memory::SharedLimits::default()).unwrap());
    let mappings = Arc::new(hl_memory::MappingCoordinator::with_shared_space(
        MappingHostAdapter::new(Arc::clone(&arena)),
        shared,
        hl_memory::AddressSpaceId { slot: 1, generation: 1 },
    ));
    let assembly = RuntimeAssembly::new(Default::default()).unwrap();
    let router = GuestExecutor::router(
        arena,
        mappings,
        &RuntimeLaunchPlan {
            rootfs: None,
            executable_host: None,
            arguments: Vec::new(),
            environment: Vec::new(),
            result_path: None,
            options: Options::default(),
        },
        &assembly,
        GuestArchitecture::X86_64,
        Arc::new(super::readiness::Cancellation::new().unwrap()),
    )
    .unwrap();
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let mut cpu = match architecture {
            GuestArchitecture::Aarch64 => {
                let mut cpu = Aarch64CpuState::default();
                cpu.registers[8] = 471;
                ExecutionCpuSnapshot::Aarch64(cpu)
            }
            GuestArchitecture::X86_64 => {
                let mut cpu = CpuState::default();
                cpu.registers[0] = 471;
                ExecutionCpuSnapshot::X86_64(cpu)
            }
        };
        assert_eq!(router.dispatch(architecture, &mut cpu), RuntimeTrapOutcome::Continue,);
        let result = match cpu {
            ExecutionCpuSnapshot::Aarch64(cpu) => cpu.registers[0],
            ExecutionCpuSnapshot::X86_64(cpu) => cpu.registers[0],
        };
        assert_eq!(result, LinuxResult::Error(hl_linux::Errno::ENOSYS).encode());
    }
}
