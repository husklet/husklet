//! Two of these four cases read `--backend-receipt`, which executes no guest at all, and the other
//! two run a three-instruction static guest to its `exit` syscall -- which the x86-64 host serves
//! through the interpreter dispatch seam exactly as the aarch64 host serves it through the JIT.
//!
//! The file used to carry `#![cfg(all(target_os = "linux", target_arch = "aarch64"))]`, and it is
//! the only file in `src/apps/engine/tests/`, so the whole integration-test target of the `engine`
//! app compiled to zero tests on any other host and reported `test result: ok. 0 passed`. No
//! runner in `.github/workflows/` is both Linux and aarch64 -- CI's Linux job is `ubuntu-24.04`
//! and its aarch64 job is `macos-26` -- so that combination selected no host at all and these four
//! cases had never run anywhere. Re-derived on x86-64 Linux: `hl-aarch64` exits 42 on the aarch64
//! fixture, `hl-x86_64` exits 43 on the x86-64 one, and both receipts verify.
//!
//! The Linux gate that remains is deliberate and unverified rather than known-necessary: the
//! fixtures encode a Linux `exit` and the macOS host is not reachable from here, so the arm that
//! could not be measured announces itself instead of vanishing.

#[cfg(target_os = "linux")]
mod linux_host {
    use sha2::Digest as _;
    use std::{fs, process::Command};

    const LINK_BASE: u64 = 0x40_0000;
    const ENTRY_OFFSET: usize = 0x180;

    fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn static_aarch64(syscall_number: u16, status: u32) -> Vec<u8> {
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
        // movz x0, #<status low 16>; movk x0, #<status high 16>, lsl 16; movz x8, #<nr>; svc #0.
        // The status is assembled in two halves so a fixture can hand the guest a value the kernel
        // must truncate, which a single `movz` of 16 bits could not express.
        put_u32(&mut bytes, ENTRY_OFFSET, 0xd280_0000 | (status & 0xffff) << 5);
        put_u32(&mut bytes, ENTRY_OFFSET + 4, 0xf2a0_0000 | (status >> 16) << 5);
        put_u32(&mut bytes, ENTRY_OFFSET + 8, 0xd280_0008 | u32::from(syscall_number) << 5);
        put_u32(&mut bytes, ENTRY_OFFSET + 12, 0xd400_0001);
        bytes
    }

    fn static_x86_64(syscall_number: u32, status: u32) -> Vec<u8> {
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
        put_u64(&mut bytes, 80, LINK_BASE);
        put_u64(&mut bytes, 88, LINK_BASE);
        let image_length = bytes.len() as u64;
        put_u64(&mut bytes, 96, image_length);
        put_u64(&mut bytes, 104, image_length);
        put_u64(&mut bytes, 112, 4096);
        // mov eax, <nr>; mov edi, <status>; syscall.
        bytes[ENTRY_OFFSET] = 0xb8;
        bytes[ENTRY_OFFSET + 1..ENTRY_OFFSET + 5].copy_from_slice(&syscall_number.to_le_bytes());
        bytes[ENTRY_OFFSET + 5] = 0xbf;
        bytes[ENTRY_OFFSET + 6..ENTRY_OFFSET + 10].copy_from_slice(&status.to_le_bytes());
        bytes[ENTRY_OFFSET + 10..ENTRY_OFFSET + 12].copy_from_slice(&[0x0f, 0x05]);
        bytes
    }

    /// `write(1, <the mapped image>, 16)` followed by `exit_group(7)`. The exit is reached only when the
    /// write returns instead of terminating the guest, so the status tells the two apart with no output
    /// to parse: 141 means SIGPIPE was delivered and took its default action, 7 means the write came
    /// back and the guest ran on.
    fn static_x86_64_write_then_exit(status: u32) -> Vec<u8> {
        let mut bytes = static_x86_64(231, status);
        let mut entry = vec![0xb8_u8, 1, 0, 0, 0, 0xbf, 1, 0, 0, 0, 0x48, 0xbe];
        entry.extend_from_slice(&LINK_BASE.to_le_bytes());
        entry.extend_from_slice(&[0xba, 16, 0, 0, 0, 0x0f, 0x05]);
        let exit = bytes[ENTRY_OFFSET..ENTRY_OFFSET + 12].to_vec();
        bytes[ENTRY_OFFSET..ENTRY_OFFSET + entry.len()].copy_from_slice(&entry);
        bytes[ENTRY_OFFSET + entry.len()..ENTRY_OFFSET + entry.len() + exit.len()].copy_from_slice(&exit);
        bytes
    }

    fn static_aarch64_write_then_exit(status: u32) -> Vec<u8> {
        let mut bytes = static_aarch64(94, status);
        let exit = bytes[ENTRY_OFFSET..ENTRY_OFFSET + 16].to_vec();
        // movz x0, #1; movz x1, #<LINK_BASE >> 16>, lsl 16; movz x2, #16; movz x8, #64; svc #0.
        for (index, word) in [0xd280_0020_u32, 0xd2a0_0801, 0xd280_0202, 0xd280_0808, 0xd400_0001]
            .into_iter()
            .enumerate()
        {
            put_u32(&mut bytes, ENTRY_OFFSET + index * 4, word);
        }
        bytes[ENTRY_OFFSET + 20..ENTRY_OFFSET + 20 + exit.len()].copy_from_slice(&exit);
        bytes
    }

    /// A pipe whose read end is already closed, handed to the guest as descriptor 1. Linux answers the
    /// write with EPIPE *and* raises SIGPIPE on the writing thread, whose default action terminates the
    /// writer -- that is what stops `cmd | head` once `head` has taken its line. The engine returned the
    /// EPIPE and raised nothing whenever the descriptor was an ADOPTED one, so `make --version | head -1`
    /// reported a write error and ran to completion, and `yes | head -1` never stopped writing. Measured
    /// on this host: the writer of `make --version | head -1` exits 1 under the old engine, 141 natively
    /// and 141 now; `yes | head -1` inside a guest bash already read 141, because a pipe the GUEST opens
    /// takes the retained descriptor path, which had the delivery all along.
    ///
    /// The engine reports a fatally signalled guest as `_exit(128 + signo)` rather than dying by the
    /// signal itself -- measured here as 139 for `raise(SIGSEGV)` and 143 for `raise(SIGTERM)` against a
    /// native -11 and -15 -- so 141 is the status this boundary can express. A guest waiting on a guest
    /// reconstructs `WIFSIGNALED` from the engine's own relay: in-guest `bash` reports
    /// `PIPESTATUS[0]` as 141, exactly as the host shell does.
    fn guest_stdout_is_a_pipe_with_no_reader(binary: &str, image: Vec<u8>, name: &str) -> Option<i32> {
        let path = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
        fs::write(&path, image).unwrap();
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let status = Command::new(binary)
            .arg(&path)
            .stdout(writer)
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        fs::remove_file(&path).unwrap();
        status.code()
    }

    #[test]
    fn direct_aarch64_worker_defaults_to_retained_c() {
        let path = std::env::temp_dir().join(format!("hl-retained-direct-{}", std::process::id()));
        fs::write(&path, static_aarch64(93, 42)).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_hl-aarch64"))
            .arg(&path)
            .output()
            .unwrap();
        fs::remove_file(path).unwrap();
        assert_eq!(output.status.code(), Some(42));
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }

    #[test]
    fn direct_x86_64_worker_defaults_to_retained_c() {
        let path = std::env::temp_dir().join(format!("hl-retained-direct-x86-{}", std::process::id()));
        fs::write(&path, static_x86_64(60, 43)).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_hl-x86_64"))
            .arg(&path)
            .output()
            .unwrap();
        fs::remove_file(path).unwrap();
        assert_eq!(output.status.code(), Some(43));
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }

    /// Linux does not preserve an `exit_group` status: `do_group_exit` stores `(status & 0xff) << 8`,
    /// so a waiter observes the low eight bits and nothing else. Measured on this host's kernel with a
    /// glibc `exit(n)` -- which issues `exit_group` -- `exit(-1)` is waited as 255, `exit(-2)` as 254,
    /// `exit(300)` as 44 and `exit(256)` as 0. The engine published the raw status instead, so the
    /// parent's consistency check found a record saying 0xffffffff beside a wait saying 255, rejected
    /// the pair as `HL_STATUS_CORRUPT`, and the runner reported 125 for every status outside 0..=255.
    /// `exit(255)` was unaffected, which is why the defect looked rare.
    ///
    /// 200 is the control: it is inside the range, so it reads 200 both before and after the fix. A
    /// truncation applied too widely -- or an assertion that merely accepts whatever the engine says --
    /// cannot pass the four out-of-range rows and this one at the same time.
    fn exit_group_status_rows() -> [(u32, i32); 5] {
        [(0xffff_ffff, 255), (0xffff_fffe, 254), (300, 44), (256, 0), (200, 200)]
    }

    #[test]
    fn an_x86_64_guest_exit_group_status_is_reported_as_the_kernel_truncates_it() {
        for (status, expected) in exit_group_status_rows() {
            let path = std::env::temp_dir()
                .join(format!("hl-exit-group-x86-{status}-{}", std::process::id()));
            fs::write(&path, static_x86_64(231, status)).unwrap();
            let output = Command::new(env!("CARGO_BIN_EXE_hl-x86_64"))
                .arg(&path)
                .output()
                .unwrap();
            fs::remove_file(&path).unwrap();
            assert_eq!(
                output.status.code(),
                Some(expected),
                "x86-64 guest exit_group({status:#x}) must be waited as {expected}, as the host kernel does; \
                 stderr={}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn an_aarch64_guest_exit_group_status_is_reported_as_the_kernel_truncates_it() {
        for (status, expected) in exit_group_status_rows() {
            let path = std::env::temp_dir()
                .join(format!("hl-exit-group-a64-{status}-{}", std::process::id()));
            fs::write(&path, static_aarch64(94, status)).unwrap();
            let output = Command::new(env!("CARGO_BIN_EXE_hl-aarch64"))
                .arg(&path)
                .output()
                .unwrap();
            fs::remove_file(&path).unwrap();
            assert_eq!(
                output.status.code(),
                Some(expected),
                "aarch64 guest exit_group({status:#x}) must be waited as {expected}, as the host kernel does; \
                 stderr={}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn an_x86_64_guest_writing_to_an_adopted_pipe_with_no_reader_is_terminated_by_sigpipe() {
        assert_eq!(
            guest_stdout_is_a_pipe_with_no_reader(
                env!("CARGO_BIN_EXE_hl-x86_64"),
                static_x86_64_write_then_exit(7),
                "hl-sigpipe-x86"
            ),
            Some(141),
            "an x86-64 guest whose adopted stdout has no reader must be terminated by SIGPIPE, not \
             handed EPIPE and left running"
        );
    }

    #[test]
    fn an_aarch64_guest_writing_to_an_adopted_pipe_with_no_reader_is_terminated_by_sigpipe() {
        assert_eq!(
            guest_stdout_is_a_pipe_with_no_reader(
                env!("CARGO_BIN_EXE_hl-aarch64"),
                static_aarch64_write_then_exit(7),
                "hl-sigpipe-a64"
            ),
            Some(141),
            "an aarch64 guest whose adopted stdout has no reader must be terminated by SIGPIPE, not \
             handed EPIPE and left running"
        );
    }

    /// The control the SIGPIPE cases need: the same two images, the same adopted descriptor 1, but a
    /// writable one. The write returns, `exit_group(7)` is reached, and the runner reports 7. Without
    /// this row a fix that terminated every writer -- or an assertion that only ever saw 141 -- would
    /// read exactly as green.
    #[test]
    fn the_same_guests_reach_their_exit_when_the_adopted_descriptor_accepts_the_write() {
        for (binary, image, name) in [
            (env!("CARGO_BIN_EXE_hl-x86_64"), static_x86_64_write_then_exit(7), "hl-sigpipe-ok-x86"),
            (env!("CARGO_BIN_EXE_hl-aarch64"), static_aarch64_write_then_exit(7), "hl-sigpipe-ok-a64"),
        ] {
            let path = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
            fs::write(&path, image).unwrap();
            let output = Command::new(binary).arg(&path).output().unwrap();
            fs::remove_file(&path).unwrap();
            assert_eq!(
                output.status.code(),
                Some(7),
                "{name}: a write an adopted descriptor accepts must return and let the guest exit; \
                 stderr={}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn receipt_is_machine_readable_and_hash_bound() {
        let binary = env!("CARGO_BIN_EXE_hl-aarch64");
        let output = Command::new(binary).arg("--backend-receipt").output().unwrap();
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(receipt["schema"], "husklet-engine-backend-v1");
        assert_eq!(receipt["backend"], "retained-c");
        let expected = sha2::Sha256::digest(fs::read(binary).unwrap());
        let expected = expected.iter().fold(String::new(), |mut output, byte| {
            use std::fmt::Write as _;
            write!(output, "{byte:02x}").expect("writing to a String cannot fail");
            output
        });
        assert_eq!(receipt["engine_sha256"], expected);
    }

    #[test]
    fn receipt_accepts_both_guests_and_rejects_retired_selector_arguments() {
        let aarch64 = env!("CARGO_BIN_EXE_hl-aarch64");
        let rejected = Command::new(aarch64)
            .args(["--backend-receipt", "--engine-option", "HL_EXECUTION_BACKEND=c"])
            .output()
            .unwrap();
        assert_eq!(rejected.status.code(), Some(125));
        assert!(rejected.stdout.is_empty());

        let x86 = Command::new(env!("CARGO_BIN_EXE_hl-x86_64"))
            .arg("--backend-receipt")
            .output()
            .unwrap();
        assert!(x86.status.success());
        assert!(x86.stderr.is_empty());
        let receipt: serde_json::Value = serde_json::from_slice(&x86.stdout).unwrap();
        assert_eq!(receipt["backend"], "retained-c");

        let unsupported_guest = Command::new(env!("CARGO_BIN_EXE_hl-engine"))
            .args(["--backend-receipt", "--guest-isa", "riscv64"])
            .output()
            .unwrap();
        assert_eq!(unsupported_guest.status.code(), Some(125));
        assert!(unsupported_guest.stdout.is_empty());
        assert!(unsupported_guest.stderr.is_empty());
    }
}

/// A gated-out file is indistinguishable from a passing one in the harness output, which is exactly
/// how the aarch64 gate above survived unnoticed. Name the uncovered host out loud instead.
///
/// The notice goes to the real stderr descriptor rather than through `eprintln!`, because libtest
/// captures Rust-level output and prints it only for a FAILING test -- the same reason
/// `hl-native`'s `guest_compiler_present` skip notice writes to descriptor 2.
#[cfg(not(target_os = "linux"))]
#[test]
fn retained_direct_worker_cases_are_uncovered_on_this_host() {
    let notice = "SKIP retained_direct_linux: the `engine` app's entire integration-test target is \
                  UNCOVERED on this host -- its four retained-C direct-worker cases need Linux.\n";
    // SAFETY: a write of a `'static` initialized buffer to the process's stderr descriptor. It
    // borrows nothing beyond the call, and a short or failed write is not an error worth acting on.
    #[allow(unsafe_code)]
    unsafe {
        libc::write(2, notice.as_ptr().cast(), notice.len());
    }
}
