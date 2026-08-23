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

/// Collects the guest's two standard streams separately: standard output is the answer being compared,
/// and standard error carries the engine's `[prof]` report, which is where the backend says whether it
/// ran.
#[derive(Default)]
struct CapturedOutput {
    out: Mutex<Vec<u8>>,
    err: Mutex<Vec<u8>>,
}

impl StandardStreamPort for CapturedOutput {
    fn write(&self, stream: StandardStream, input: &[u8]) -> std::io::Result<usize> {
        match stream {
            StandardStream::Stdout => self.out.lock().unwrap().extend_from_slice(input),
            StandardStream::Stderr => self.err.lock().unwrap().extend_from_slice(input),
        }
        Ok(input.len())
    }

    fn close(&self) {}
}

/// What the engine reported about the same-ISA backend for one run.
struct Backend {
    line: String,
    entries: u64,
}

/// Parses the `[prof] translit: ...` line the exit report emits under `HL_C_DIAGNOSTICS`.
fn backend(stderr: &[u8]) -> Backend {
    let text = String::from_utf8_lossy(stderr);
    let line = text
        .lines()
        .find(|line| line.starts_with("[prof] translit:"))
        .unwrap_or_else(|| {
            panic!("the exit report carried no translit line; HL_C_DIAGNOSTICS produced:\n{text}");
        })
        .to_owned();
    let entries = line
        .split_whitespace()
        .find_map(|field| field.strip_prefix("entries="))
        .and_then(|value| value.trim_end_matches(')').parse().ok())
        .unwrap_or(0);
    Backend { line, entries }
}

/// Builds one fixture position-independent and statically linked.
fn fixture(directory: &Path, name: &str) -> PathBuf {
    let output = build(directory, name, "-static-pie");
    assert!(
        elf_is_position_independent(&output),
        "{name} is not ET_DYN: translit_image_ok() declines a non-PIE image outright, so a non-PIE \
         fixture would compare the interpreter against itself"
    );
    output
}

/// Builds one fixture as a non-PIE `ET_EXEC`, which is the shape the image refusal is about.
fn displaced_fixture(directory: &Path, name: &str) -> PathBuf {
    let output = build(directory, name, "-static");
    assert!(
        !elf_is_position_independent(&output),
        "{name} is not ET_EXEC, so it does not exercise the non-PIE image refusal at all"
    );
    output
}

fn build(directory: &Path, name: &str, linkage: &str) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/translit")
        .join(format!("{name}.c"));
    let output = directory.join(format!("{name}{linkage}"));
    let compiler = "x86_64-linux-gnu-gcc";
    let status = std::process::Command::new(compiler)
        .args([
            linkage,
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
    output
}

/// `e_type == ET_DYN`, read straight out of the ELF header.
fn elf_is_position_independent(path: &Path) -> bool {
    let mut header = [0u8; 20];
    let mut file = std::fs::File::open(path).expect("fixture");
    file.read_exact(&mut header).expect("ELF header");
    header[..4] == [0x7f, b'E', b'L', b'F'] && u16::from_le_bytes([header[16], header[17]]) == 3
}

/// One guest run with the backend explicitly selected -- through the LAUNCH OPTION, never through the
/// environment, so this gate does not depend on `translit_enabled()`'s command-line fallback existing.
/// Answers (stdout, exit status, what the backend reported about itself).
fn run(executable: &Path, translit: &str) -> (Vec<u8>, i32, Backend) {
    let captured = Arc::new(CapturedOutput::default());
    let mut options = Options::default();
    options.set("HL_TRANSLIT", translit, true).expect("HL_TRANSLIT");
    options.set("HL_C_DIAGNOSTICS", "1", true).expect("HL_C_DIAGNOSTICS");
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
    let out = captured.out.lock().unwrap().clone();
    let report = backend(&captured.err.lock().unwrap());
    (out, exit.guest_status, report)
}

/// The whole contract: the backend selection must not be observable in the guest's output.
fn agrees(name: &str) {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), name);
    let (interpreted, interpreted_status, interpreted_backend) = run(&executable, "0");
    let (transliterated, transliterated_status, transliterated_backend) = run(&executable, "1");
    // The counter, not the option. Setting HL_TRANSLIT proves the launch asked for the backend; only
    // `entries` proves a block of this guest actually ran as emitted host code. Without it every case
    // in this file would pass against a build in which the backend never engaged -- which is the state
    // this repository was in for the whole life of the file under test.
    assert_eq!(
        interpreted_backend.line, "[prof] translit: not selected",
        "{name}: the interpreter arm reported {}",
        interpreted_backend.line
    );
    assert!(
        transliterated_backend.entries > 0,
        "{name}: the transliterator arm entered no emitted block -- {}",
        transliterated_backend.line
    );
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
    agrees("operands");
}

/// The other refusal, and the one that decides whether this backend is worth anything to a developer.
///
/// A single anonymous `PROT_EXEC` mapping latches `g_rwx_guest`, and nothing clears it -- not a later
/// `mprotect`, not `execve`. Every JIT-hosting guest takes that mapping within milliseconds of starting,
/// so a JVM, V8, .NET or `LuaJIT` workload runs entirely interpreted with the option on and nothing says
/// so. This case exists to keep that fact attached to a number rather than to a memory: it asserts the
/// refusal is reported, that it is the executable-mapping one and not the image one, and that the
/// answer is unchanged either way.
#[test]
fn an_executable_guest_mapping_refuses_the_backend_for_the_rest_of_the_process() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "executable_mapping");
    let (interpreted, interpreted_status, _) = run(&executable, "0");
    let (selected, selected_status, selected_backend) = run(&executable, "1");
    assert!(
        selected_backend
            .line
            .contains("declined, guest executable mapping or shared alias observed"),
        "an anonymous PROT_EXEC mapping no longer refuses the backend -- {}",
        selected_backend.line
    );
    assert!(
        selected_backend.entries > 0,
        "the run before the mapping should still have entered emitted code -- {}",
        selected_backend.line
    );
    assert_eq!(
        interpreted_status, selected_status,
        "the refusal changed the exit status"
    );
    assert_eq!(
        String::from_utf8_lossy(&interpreted),
        String::from_utf8_lossy(&selected),
        "the refusal changed the answer"
    );
}

/// The image refusal, which is the one part of `translit.inc` that must fire rather than work.
///
/// A non-PIE `ET_EXEC` is mapped at a storage bias whenever the loader could not place it at its link
/// address, and every baked pointer in it then has two names. Verbatim copying can express neither
/// direction of that fold, so `translit_image_ok()` answers false for the whole image and the block runs
/// on the interpreter, which already implements both. Selecting the backend must therefore be a no-op
/// here rather than a miscompile: with the `g_nonpie_lo == 0` clamp removed, the same guests take a
/// SIGSEGV inside the engine.
#[test]
fn a_non_position_independent_image_is_refused_rather_than_transliterated() {
    let work = TempDir::new().unwrap();
    for name in ["flags", "operands", "sigs"] {
        let executable = displaced_fixture(work.path(), name);
        let (interpreted, interpreted_status, _) = run(&executable, "0");
        let (selected, selected_status, selected_backend) = run(&executable, "1");
        assert_eq!(
            interpreted_status, 0,
            "{name}: the non-PIE fixture did not run under the interpreter"
        );
        // Named, not merely absent: the refusal has to be the IMAGE predicate, not a filter that
        // happened to admit nothing, and an operator has no other way to tell the two apart.
        assert!(
            selected_backend
                .line
                .contains("declined, non-PIE image at a storage bias"),
            "{name}: the backend did not report the non-PIE image refusal -- {}",
            selected_backend.line
        );
        assert_eq!(
            selected_backend.entries, 0,
            "{name}: a refused image still entered emitted code"
        );
        assert_eq!(
            selected_status, interpreted_status,
            "{name}: selecting the transliterator changed the exit status of a refused image"
        );
        assert_eq!(
            String::from_utf8_lossy(&interpreted),
            String::from_utf8_lossy(&selected),
            "{name}: selecting the transliterator changed the output of a refused image"
        );
    }
}
