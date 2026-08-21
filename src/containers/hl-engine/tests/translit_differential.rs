#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! The same-ISA x86-64 transliterator against the interpreter, from one binary.
//!
//! `translit.inc` is a second execution backend for an x86-64 guest on an x86-64 Linux host: straight-line
//! guest instructions are copied into the code cache verbatim and only block terminators and RIP-relative
//! displacements are rewritten. It is selected by the `HL_TRANSLIT` launch option and it is additive --
//! `host_entry_off == 0` means "interpret this block", both kinds live in one cache, and anything the
//! filter declines leaves the block to the interpreter.
//!
//! That additivity is exactly what makes it hard to test: a wrong answer from a copied instruction is not
//! a crash, it is a different number. So every case here runs the SAME guest image twice through the same
//! engine, once with `HL_TRANSLIT=0` and once with `HL_TRANSLIT=1`, and requires byte-identical output and
//! the same exit status. The interpreter is the oracle.
//!
//! **A fixture must be position-independent or it proves nothing.** `translit_image_ok()` refuses the
//! whole image when `g_nonpie_lo != 0`, and on this Linux host a non-PIE `ET_EXEC` guest is placed at a
//! storage bias rather than at its link address -- measured, not assumed: with `-static` fixtures the
//! instruction count is identical to four significant figures with the option on and off, and removing the
//! `g_nonpie_lo == 0` clamp makes the engine SIGSEGV on the same guests. Every fixture is therefore built
//! `-static-pie` and `elf_is_position_independent` asserts it, because a `-static` fixture would pass this
//! whole file while executing no transliterated instruction at all.

use hl_engine::{
    activation::GuestIsa,
    composition::{StandardStream, StandardStreamPort, StandardStreams},
    launcher::plan::RuntimePlan,
    options::Options,
    runtime::Engine,
};
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

/// Collects everything the guest writes to its standard streams, in order.
#[derive(Default)]
struct CapturedOutput {
    bytes: Mutex<Vec<u8>>,
}

impl StandardStreamPort for CapturedOutput {
    fn write(&self, _: StandardStream, input: &[u8]) -> std::io::Result<usize> {
        self.bytes.lock().unwrap().extend_from_slice(input);
        Ok(input.len())
    }

    fn close(&self) {}
}

/// Builds one fixture position-independent and statically linked.
fn fixture(directory: &Path, name: &str) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/translit")
        .join(format!("{name}.c"));
    let output = directory.join(name);
    let compiler = "x86_64-linux-gnu-gcc";
    let status = std::process::Command::new(compiler)
        .args([
            "-static-pie",
            "-O2",
            "-fno-optimize-sibling-calls",
            "-z",
            "noexecstack",
            "-pthread",
            "-o",
        ])
        .arg(&output)
        .arg(&source)
        .status()
        .unwrap_or_else(|error| panic!("cannot run {compiler}: {error}"));
    assert!(status.success(), "{compiler} failed on {name} with {status}");
    assert!(
        elf_is_position_independent(&output),
        "{name} is not ET_DYN: translit_image_ok() declines a non-PIE image outright, so a non-PIE \
         fixture would compare the interpreter against itself"
    );
    output
}

/// `e_type == ET_DYN`, read straight out of the ELF header.
fn elf_is_position_independent(path: &Path) -> bool {
    let mut header = [0u8; 20];
    let mut file = std::fs::File::open(path).expect("fixture");
    file.read_exact(&mut header).expect("ELF header");
    header[..4] == [0x7f, b'E', b'L', b'F'] && u16::from_le_bytes([header[16], header[17]]) == 3
}

/// One guest run with the backend explicitly selected. Answers (stdout, exit status).
fn run(executable: &Path, translit: &str) -> (Vec<u8>, i32) {
    let captured = Arc::new(CapturedOutput::default());
    let mut options = Options::default();
    options.set("HL_TRANSLIT", translit, true).expect("HL_TRANSLIT");
    let plan = RuntimePlan {
        rootfs: None,
        executable_host: Some(executable.as_os_str().as_encoded_bytes().to_vec()),
        arguments: vec![executable.as_os_str().as_encoded_bytes().to_vec()],
        environment: Vec::new(),
        result_path: None,
        options,
    };
    let streams = StandardStreams::default().with_output(captured.clone());
    let engine = Engine::with_streams(GuestIsa::X86_64, plan, streams).expect("launch");
    engine.start().expect("start");
    let exit = engine.wait().expect("wait");
    engine.destroy().expect("destroy");
    let bytes = captured.bytes.lock().unwrap().clone();
    (bytes, exit.guest_status)
}

/// The whole contract: the backend selection must not be observable in the guest's output.
fn agrees(name: &str) {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), name);
    let (interpreted, interpreted_status) = run(&executable, "0");
    let (transliterated, transliterated_status) = run(&executable, "1");
    assert_eq!(
        interpreted_status, transliterated_status,
        "{name}: exit status differs between the interpreter and the transliterator"
    );
    assert_eq!(
        String::from_utf8_lossy(&interpreted),
        String::from_utf8_lossy(&transliterated),
        "{name}: output differs between the interpreter and the transliterator"
    );
    assert!(
        !interpreted.is_empty(),
        "{name} produced no output at all under either backend"
    );
    // The third oracle. Two engine arms that agree can still both be wrong -- and every value these
    // fixtures print is algorithmic, so the host itself, being an x86-64 Linux machine, computes the
    // same answer. Without this the whole file would pass against an engine that had stopped executing
    // the fixture and printed a constant.
    let native = std::process::Command::new(&executable)
        .arg0(&executable)
        .output()
        .expect("the fixture runs on this host directly");
    assert_eq!(
        String::from_utf8_lossy(&native.stdout),
        String::from_utf8_lossy(&interpreted),
        "{name}: the engine disagrees with the host running the same image natively"
    );
}

/// Flag round-trip across block boundaries, including the PF byte-parity encoding.
///
/// Inverting `translit_flags_out`'s PF polarity reddens exactly this case and leaves every real guest
/// program in the corpus byte-identical.
#[test]
fn flag_state_survives_every_transliterated_block_boundary() {
    agrees("flags");
}

/// A guest that writes its own code at runtime.
#[test]
fn a_guest_that_generates_code_at_runtime_agrees_with_the_interpreter() {
    agrees("smc");
}

/// Faults into transliterated frames, including a guest stack overflow onto the alternate stack.
#[test]
fn signals_delivered_into_transliterated_frames_agree_with_the_interpreter() {
    agrees("sigs");
}

/// `%gs` republication for a cloned thread, a fork child, a vfork+execve and a raw clone.
#[test]
fn threads_fork_and_exec_agree_with_the_interpreter() {
    agrees("procs");
}

/// RIP-relative operands, indirect terminators, string operations and deep call/ret.
#[test]
fn operand_and_terminator_coverage_agrees_with_the_interpreter() {
    agrees("misc");
}
