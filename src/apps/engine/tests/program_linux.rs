#![cfg(target_os = "linux")]

use std::fs;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FILE: AtomicU64 = AtomicU64::new(1);
const LINK_BASE: u64 = 0x40_0000;
const ENTRY_OFFSET: usize = 0x180;
const LAUNCH_HEADER_SIZE: usize = 192;

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn static_x86(status: u8) -> Vec<u8> {
    let mut bytes = vec![0_u8; 4096];
    bytes[..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2;
    bytes[5] = 1;
    bytes[6] = 1;
    put_u16(&mut bytes, 16, 2);
    put_u16(&mut bytes, 18, 62);
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
    bytes[ENTRY_OFFSET..ENTRY_OFFSET + 7].copy_from_slice(&[0xb8, 60, 0, 0, 0, 0xbf, status]);
    bytes[ENTRY_OFFSET + 7..ENTRY_OFFSET + 12].copy_from_slice(&[0, 0, 0, 0x0f, 0x05]);
    bytes
}

fn futex_x86(operation: u32, value: u32, timeout: u8) -> Vec<u8> {
    let mut bytes = static_x86(0);
    let mut code = Vec::new();
    code.push(0xbf);
    code.extend_from_slice(&(LINK_BASE as u32 + 0x300).to_le_bytes());
    code.push(0xbe);
    code.extend_from_slice(&operation.to_le_bytes());
    code.push(0xba);
    code.extend_from_slice(&value.to_le_bytes());
    code.extend_from_slice(&[0x41, 0xba]);
    let timeout_address = match timeout {
        1 => LINK_BASE as u32 + 0x308,
        2 => 0x50_0000,
        _ => 0,
    };
    code.extend_from_slice(&timeout_address.to_le_bytes());
    code.extend_from_slice(&[0xb8, 202, 0, 0, 0, 0x0f, 0x05]);
    code.extend_from_slice(&[0x89, 0xc7, 0xb8, 60, 0, 0, 0, 0x0f, 0x05]);
    bytes[ENTRY_OFFSET..ENTRY_OFFSET + code.len()].copy_from_slice(&code);
    if timeout == 1 {
        bytes[0x310..0x318].copy_from_slice(&1_000_000_u64.to_le_bytes());
    }
    bytes
}

fn futex_arm(operation: u16, value: u16, timeout: u8) -> Vec<u8> {
    let mut bytes = vec![0_u8; 4096];
    bytes[..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2;
    bytes[5] = 1;
    bytes[6] = 1;
    put_u16(&mut bytes, 16, 2);
    put_u16(&mut bytes, 18, 183);
    put_u32(&mut bytes, 20, 1);
    put_u64(&mut bytes, 24, LINK_BASE + ENTRY_OFFSET as u64);
    put_u64(&mut bytes, 32, 64);
    put_u16(&mut bytes, 52, 64);
    put_u16(&mut bytes, 54, 56);
    put_u16(&mut bytes, 56, 1);
    put_u32(&mut bytes, 64, 1);
    put_u32(&mut bytes, 68, 5);
    put_u64(&mut bytes, 80, LINK_BASE);
    put_u64(&mut bytes, 88, LINK_BASE);
    let image_length = bytes.len() as u64;
    put_u64(&mut bytes, 96, image_length);
    put_u64(&mut bytes, 104, image_length);
    put_u64(&mut bytes, 112, 4096);
    for (index, instruction) in [
        0xd280_6000_u32,
        0xf2a0_0800_u32,
        0xd280_0001_u32 | u32::from(operation) << 5,
        0xd280_0002_u32 | u32::from(value) << 5,
        if timeout == 1 { 0xd280_6103_u32 } else { 0xd280_0003_u32 },
        match timeout {
            1 => 0xf2a0_0803_u32,
            2 => 0xf2a0_0a03_u32,
            _ => 0xd503_201f_u32,
        },
        0xd280_0c48_u32,
        0xd400_0001_u32,
        0xd280_0ba8_u32,
        0xd400_0001_u32,
    ]
    .into_iter()
    .enumerate()
    {
        put_u32(&mut bytes, ENTRY_OFFSET + index * 4, instruction);
    }
    if timeout == 1 {
        bytes[0x310..0x318].copy_from_slice(&1_000_000_u64.to_le_bytes());
    }
    bytes
}

fn routed_x86() -> Vec<u8> {
    const MESSAGE_OFFSET: usize = 0x220;
    let mut bytes = static_x86(0);
    let address = (LINK_BASE + MESSAGE_OFFSET as u64) as u32;
    let mut code = Vec::new();
    code.extend_from_slice(&[0xb8, 1, 0, 0, 0]);
    code.extend_from_slice(&[0xbf, 1, 0, 0, 0]);
    code.push(0xbe);
    code.extend_from_slice(&address.to_le_bytes());
    code.extend_from_slice(&[0xba, 5, 0, 0, 0, 0x0f, 0x05]);
    code.extend_from_slice(&[0x89, 0xc7, 0xb8, 60, 0, 0, 0, 0x0f, 0x05]);
    bytes[ENTRY_OFFSET..ENTRY_OFFSET + code.len()].copy_from_slice(&code);
    bytes[MESSAGE_OFFSET..MESSAGE_OFFSET + 5].copy_from_slice(b"rust\n");
    bytes
}

fn split_image(kind: u16, link_base: u64, status: u32) -> Vec<u8> {
    let data_address = link_base + 4096;
    let mut bytes = vec![0_u8; 8192];
    bytes[..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2;
    bytes[5] = 1;
    bytes[6] = 1;
    put_u16(&mut bytes, 16, kind);
    put_u16(&mut bytes, 18, 62);
    put_u32(&mut bytes, 20, 1);
    put_u64(&mut bytes, 24, link_base + ENTRY_OFFSET as u64);
    put_u64(&mut bytes, 32, 64);
    put_u16(&mut bytes, 52, 64);
    put_u16(&mut bytes, 54, 56);
    put_u16(&mut bytes, 56, 2);

    put_u32(&mut bytes, 64, 1);
    put_u32(&mut bytes, 68, 5);
    put_u64(&mut bytes, 72, 0);
    put_u64(&mut bytes, 80, link_base);
    put_u64(&mut bytes, 88, link_base);
    put_u64(&mut bytes, 96, 4096);
    put_u64(&mut bytes, 104, 4096);
    put_u64(&mut bytes, 112, 4096);

    let data_header = 64 + 56;
    put_u32(&mut bytes, data_header, 1);
    put_u32(&mut bytes, data_header + 4, 6);
    put_u64(&mut bytes, data_header + 8, 4096);
    put_u64(&mut bytes, data_header + 16, data_address);
    put_u64(&mut bytes, data_header + 24, data_address);
    put_u64(&mut bytes, data_header + 32, 4);
    put_u64(&mut bytes, data_header + 40, 4096);
    put_u64(&mut bytes, data_header + 48, 4096);

    let entry = link_base + ENTRY_OFFSET as u64;
    let next = entry + 6;
    let displacement = i32::try_from(data_address - next).unwrap();
    bytes[ENTRY_OFFSET..ENTRY_OFFSET + 2].copy_from_slice(&[0x8b, 0x3d]);
    bytes[ENTRY_OFFSET + 2..ENTRY_OFFSET + 6].copy_from_slice(&displacement.to_le_bytes());
    bytes[ENTRY_OFFSET + 6..ENTRY_OFFSET + 11].copy_from_slice(&[0xb8, 60, 0, 0, 0]);
    bytes[ENTRY_OFFSET + 11..ENTRY_OFFSET + 13].copy_from_slice(&[0x0f, 0x05]);
    bytes[4096..4100].copy_from_slice(&status.to_le_bytes());
    bytes
}

fn split_x86(status: u32) -> Vec<u8> {
    split_image(2, LINK_BASE, status)
}

fn dynamic_main(interpreter: &[u8]) -> Vec<u8> {
    let interpreter_offset = 0x200;
    let mut bytes = static_x86(0);
    put_u16(&mut bytes, 16, 3);
    put_u16(&mut bytes, 56, 2);
    let header = 64 + 56;
    put_u32(&mut bytes, header, 3);
    put_u64(&mut bytes, header + 8, interpreter_offset);
    put_u64(&mut bytes, header + 32, interpreter.len() as u64 + 1);
    put_u64(&mut bytes, header + 40, interpreter.len() as u64 + 1);
    put_u64(&mut bytes, header + 48, 1);
    let offset = usize::try_from(interpreter_offset).unwrap();
    bytes[offset..offset + interpreter.len()].copy_from_slice(interpreter);
    bytes[offset + interpreter.len()] = 0;
    bytes
}

fn launch_wire(executable: &[u8]) -> Vec<u8> {
    launch_wire_root(executable, None)
}

fn launch_wire_root(executable: &[u8], rootfs: Option<&[u8]>) -> Vec<u8> {
    let root_size = rootfs.map_or(0, |root| root.len() + 1);
    let pool_size = root_size + executable.len() + 3;
    let mut wire = vec![0_u8; LAUNCH_HEADER_SIZE + pool_size];
    put_u32(&mut wire, 0, 0x484c_4346);
    put_u32(&mut wire, 4, pool_size as u32);
    put_u32(&mut wire, 8, LAUNCH_HEADER_SIZE as u32);
    put_u32(&mut wire, 12, 1);
    let executable_offset = 1 + root_size;
    if let Some(root) = rootfs {
        put_u32(&mut wire, 56, 1);
        let root_start = LAUNCH_HEADER_SIZE + 1;
        wire[root_start..root_start + root.len()].copy_from_slice(root);
    }
    put_u32(&mut wire, 108, executable_offset as u32);
    put_u64(&mut wire, 152, 1);
    put_u32(&mut wire, 168, executable_offset as u32);
    let executable_start = LAUNCH_HEADER_SIZE + executable_offset;
    wire[executable_start..executable_start + executable.len()].copy_from_slice(executable);
    wire
}

#[test]
fn executable_runs_static() {
    let identity = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("hl-engine-program-{}-{identity}", std::process::id()));
    fs::write(&path, static_x86(42)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_hl-engine"))
        .args([
            "--guest-isa",
            "x86_64",
            path.to_str().expect("temporary path is Unicode"),
        ])
        .output()
        .unwrap();
    fs::remove_file(path).unwrap();
    assert_eq!(output.status.code(), Some(42));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn futex_routes_both() {
    for (isa, image, expected) in [
        ("x86_64", futex_x86(129, 1, 0), 0),
        ("x86_64", futex_x86(128, 1, 0), 245),
        ("x86_64", futex_x86(128, 0, 1), 146),
        ("x86_64", futex_x86(128, 0, 2), 242),
        ("x86_64", futex_x86(384, 1, 0), 234),
        ("aarch64", futex_arm(129, 1, 0), 0),
        ("aarch64", futex_arm(128, 1, 0), 245),
        ("aarch64", futex_arm(128, 0, 1), 146),
        ("aarch64", futex_arm(128, 0, 2), 242),
        ("aarch64", futex_arm(384, 1, 0), 234),
    ] {
        let identity = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("hl-engine-futex-{}-{identity}", std::process::id()));
        fs::write(&path, image).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_hl-engine"))
            .args(["--guest-isa", isa, path.to_str().unwrap()])
            .output()
            .unwrap();
        fs::remove_file(path).unwrap();
        assert_eq!(output.status.code(), Some(expected), "{isa}: {:?}", output.stderr);
    }
}

#[test]
fn signal_report_optin() {
    let identity = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("hl-engine-fault-{}-{identity}", std::process::id()));
    let mut image = static_x86(0);
    image[ENTRY_OFFSET..ENTRY_OFFSET + 2].copy_from_slice(&[0x0f, 0xff]);
    fs::write(&path, image).unwrap();
    let plain = Command::new(env!("CARGO_BIN_EXE_hl-engine"))
        .args(["--guest-isa", "x86_64", path.to_str().unwrap()])
        .output()
        .unwrap();
    let report = Command::new(env!("CARGO_BIN_EXE_hl-engine"))
        .args(["--report-exit", "--guest-isa", "x86_64", path.to_str().unwrap()])
        .output()
        .unwrap();
    fs::remove_file(path).unwrap();
    assert_eq!(plain.status.code(), Some(128 + 4));
    assert!(plain.stderr.is_empty(), "{}", String::from_utf8_lossy(&plain.stderr));
    let stderr = String::from_utf8(report.stderr).unwrap();
    assert_eq!(stderr, "[hl-exit]\tSignal\t4\t0x0\n");
}

#[test]
fn routed_write_resumes() {
    let identity = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("hl-engine-router-{}-{identity}", std::process::id()));
    fs::write(&path, routed_x86()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_hl-engine"))
        .args([
            "--guest-isa",
            "x86_64",
            path.to_str().expect("temporary path is Unicode"),
        ])
        .output()
        .unwrap();
    fs::remove_file(path).unwrap();
    assert_eq!(output.status.code(), Some(5));
    assert_eq!(output.stdout, b"rust\n");
    assert!(output.stderr.is_empty());
}

#[test]
fn executable_reads_data() {
    let identity = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("hl-engine-program-split-{}-{identity}", std::process::id()));
    fs::write(&path, split_x86(73)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_hl-engine"))
        .args([
            "--guest-isa",
            "x86_64",
            path.to_str().expect("temporary path is Unicode"),
        ])
        .output()
        .unwrap();
    fs::remove_file(path).unwrap();
    assert_eq!(output.status.code(), Some(73));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn interpreter_handoff_runs() {
    let identity = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
    let stem = std::env::temp_dir().join(format!("hl-engine-interpreter-{}-{identity}", std::process::id()));
    let main = stem.with_extension("main");
    let interpreter = stem.with_extension("ld");
    fs::write(&interpreter, split_image(3, 0, 61)).unwrap();
    fs::write(
        &main,
        dynamic_main(interpreter.to_str().expect("temporary path is Unicode").as_bytes()),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_hl-engine"))
        .args([
            "--guest-isa",
            "x86_64",
            main.to_str().expect("temporary path is Unicode"),
        ])
        .output()
        .unwrap();
    fs::remove_file(main).unwrap();
    fs::remove_file(interpreter).unwrap();
    assert_eq!(output.status.code(), Some(61));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn rooted_interpreter_runs() {
    let identity = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("hl-engine-rootfs-{}-{identity}", std::process::id()));
    let main = root.with_extension("main");
    let config = root.with_extension("wire");
    fs::create_dir_all(root.join("lib")).unwrap();
    fs::write(root.join("lib/guest-ld.so"), split_image(3, 0, 47)).unwrap();
    fs::write(&main, dynamic_main(b"/lib/guest-ld.so")).unwrap();
    fs::write(
        &config,
        launch_wire_root(
            main.to_str().expect("temporary path is Unicode").as_bytes(),
            Some(root.to_str().expect("temporary path is Unicode").as_bytes()),
        ),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_hl-engine"))
        .args([
            "--configfile",
            config.to_str().expect("temporary path is Unicode"),
            "--guest-isa",
            "x86_64",
        ])
        .output()
        .unwrap();
    fs::remove_file(main).unwrap();
    fs::remove_file(config).unwrap();
    fs::remove_dir_all(root).unwrap();
    assert_eq!(output.status.code(), Some(47));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn config_route_runs() {
    let identity = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("hl-engine-config-{}-{identity}", std::process::id()));
    let guest = root.with_extension("guest");
    let config = root.with_extension("wire");
    fs::write(&guest, static_x86(37)).unwrap();
    fs::write(
        &config,
        launch_wire(guest.to_str().expect("temporary path is Unicode").as_bytes()),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_hl-engine"))
        .args([
            "--configfile",
            config.to_str().expect("temporary path is Unicode"),
            "--guest-isa",
            "x86_64",
        ])
        .output()
        .unwrap();
    fs::remove_file(guest).unwrap();
    fs::remove_file(config).unwrap();
    assert_eq!(output.status.code(), Some(37));
}

#[test]
fn unsupported_server_route() {
    let output = Command::new(env!("CARGO_BIN_EXE_hl-engine"))
        .arg("--server")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(69));
}
