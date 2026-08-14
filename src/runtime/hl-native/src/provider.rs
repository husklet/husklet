#![allow(unsafe_code)]

/// Exercises a deliberately retained native allocation so leak tooling can
/// prove that it observes the integrated C engine.
#[must_use]
pub fn leak_check_nonvacuity() -> i32 {
    // SAFETY: the symbol takes no arguments and owns its test allocation.
    unsafe { super::bindings::hl_c_backend_leak_check_nonvacuity() }
}

/// Creates, runs, observes, and destroys one engine from a minimal static image after relocation.
///
/// `scratch` must name an existing private directory owned by the caller. The
/// smoke creates its temporary guest image there and removes it before return.
#[cfg(unix)]
#[doc(hidden)]
pub fn artifact_lifecycle_smoke(scratch: &std::path::Path) -> Result<(), String> {
    use std::{
        io::Write as _,
        os::{fd::AsRawFd as _, unix::ffi::OsStrExt as _},
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut image = None;
    for attempt in 0..16 {
        let path = scratch.join(format!(
            "hl-native-artifact-smoke-{}-{}-{attempt}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::OpenOptions::new().create_new(true).write(true).open(&path) {
            Ok(mut file) => {
                file.write_all(&artifact_guest_image(std::env::consts::ARCH))
                    .map_err(|error| error.to_string())?;
                file.sync_all().map_err(|error| error.to_string())?;
                image = Some(ArtifactImage(path));
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    let image = image.ok_or_else(|| "could not reserve relocated lifecycle smoke image".to_owned())?;
    let executable = std::ffi::CString::new(image.0.as_os_str().as_bytes()).map_err(|error| error.to_string())?;
    let standard = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
        .map_err(|error| error.to_string())?;
    let isa = match std::env::consts::ARCH {
        "aarch64" => 1,
        "x86_64" => 2,
        architecture => return Err(format!("unsupported native smoke architecture {architecture}")),
    };
    let config = super::EngineConfig {
        isa,
        rootfs: None,
        executable_host: Some(&executable),
        executable_fd: -1,
        option_names: &[],
        option_values: &[],
        standard_fds: [standard.as_raw_fd(); 3],
        provider_fd: -1,
    };
    // SAFETY: every borrowed string, slice, and descriptor remains live through create.
    let engine = unsafe { super::Engine::create(config) }.map_err(|error| error.to_string())?;
    let arguments = [executable.as_ptr()];
    engine.run(&arguments).map_err(|error| error.to_string())?;
    let exit = engine.exit();
    if exit.kind != 1 || exit.status != 0 || exit.detail != 0 {
        return Err(format!(
            "relocated static smoke exited kind={} status={} detail={}",
            exit.kind, exit.status, exit.detail
        ));
    }
    drop(engine);
    Ok(())
}

#[cfg(unix)]
struct ArtifactImage(std::path::PathBuf);

#[cfg(unix)]
impl Drop for ArtifactImage {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(unix)]
fn artifact_guest_image(architecture: &str) -> Vec<u8> {
    const BASE: u64 = 0x40_0000;
    const ENTRY: usize = 0x180;
    let mut bytes = vec![0_u8; 4096];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    bytes[16..18].copy_from_slice(&2_u16.to_le_bytes());
    let machine = if architecture == "aarch64" { 183_u16 } else { 62_u16 };
    bytes[18..20].copy_from_slice(&machine.to_le_bytes());
    bytes[20..24].copy_from_slice(&1_u32.to_le_bytes());
    bytes[24..32].copy_from_slice(&(BASE + ENTRY as u64).to_le_bytes());
    bytes[32..40].copy_from_slice(&64_u64.to_le_bytes());
    bytes[52..54].copy_from_slice(&64_u16.to_le_bytes());
    bytes[54..56].copy_from_slice(&56_u16.to_le_bytes());
    bytes[56..58].copy_from_slice(&1_u16.to_le_bytes());
    bytes[64..68].copy_from_slice(&1_u32.to_le_bytes());
    bytes[68..72].copy_from_slice(&5_u32.to_le_bytes());
    bytes[80..88].copy_from_slice(&BASE.to_le_bytes());
    bytes[88..96].copy_from_slice(&BASE.to_le_bytes());
    bytes[96..104].copy_from_slice(&4096_u64.to_le_bytes());
    bytes[104..112].copy_from_slice(&4096_u64.to_le_bytes());
    bytes[112..120].copy_from_slice(&4096_u64.to_le_bytes());
    if architecture == "aarch64" {
        bytes[ENTRY..ENTRY + 4].copy_from_slice(&0xd280_0000_u32.to_le_bytes());
        bytes[ENTRY + 4..ENTRY + 8].copy_from_slice(&0xd280_0ba8_u32.to_le_bytes());
        bytes[ENTRY + 8..ENTRY + 12].copy_from_slice(&0xd400_0001_u32.to_le_bytes());
    } else {
        bytes[ENTRY..ENTRY + 12].copy_from_slice(&[0xb8, 60, 0, 0, 0, 0xbf, 0, 0, 0, 0, 0x0f, 0x05]);
    }
    bytes
}
