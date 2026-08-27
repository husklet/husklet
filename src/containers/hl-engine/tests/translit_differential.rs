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
//! Linux places a non-PIE `ET_EXEC` guest at its link address when that range is free, so those images are
//! valid transliterator fixtures too. If anything already owns the link range, the loader safely falls back
//! to displaced storage and `translit_image_ok()` refuses the image: verbatim copied instructions cannot
//! express the two address domains. Both paths are exercised below, including a non-clobbering collision.

use hl_engine::{
    activation::GuestIsa,
    composition::{StandardStream, StandardStreamPort, StandardStreams},
    launcher::plan::RuntimePlan,
    options::Options,
    runtime::Engine,
};
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
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
    blocks: u64,
    entries: u64,
    declined: u64,
    stitch_candidates: u64,
    stitch_admitted: u64,
    jcc_fall_candidates: u64,
    jcc_fall_admitted: u64,
    jcc_fall_page_refused: u64,
    jcc_fall_successor_page_refused: u64,
    jcc_fall_executed: u64,
    operand_declined: u64,
    sse2_memory_declined: u64,
    riprel_lowered: u64,
    scratch_lowered: u64,
    lea_lowered: u64,
    abs32_lowered: u64,
    natural_lea_lowered: u64,
    rip_indirect_lowered: u64,
    provenance_fallback: u64,
    body_owner_recovered: u64,
    body_owner_published: u64,
    sse2_runs_admitted: u64,
    sse2_instructions_admitted: u64,
    sse2_target_runs: u64,
    translations: u64,
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
    let counter = |name: &str| {
        line.split_whitespace()
            .find_map(|field| field.strip_prefix(name))
            .and_then(|value| value.trim_end_matches(')').parse().ok())
            .unwrap_or(0)
    };
    let translations = text
        .lines()
        .find(|line| line.starts_with("[prof] crossings="))
        .and_then(|line| {
            line.split_whitespace()
                .find_map(|field| field.strip_prefix("translations="))
        })
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    Backend {
        blocks: counter("blocks="),
        entries: counter("entries="),
        declined: counter("declined="),
        stitch_candidates: counter("stitch_candidates="),
        stitch_admitted: counter("stitch_admitted="),
        jcc_fall_candidates: counter("jcc_fall_candidates="),
        jcc_fall_admitted: counter("jcc_fall_admitted="),
        jcc_fall_page_refused: counter("jcc_fall_page_refused="),
        jcc_fall_successor_page_refused: counter("jcc_fall_successor_page_refused="),
        jcc_fall_executed: counter("jcc_fall_executed="),
        operand_declined: counter("operand_declined="),
        sse2_memory_declined: counter("sse2_memory_declined="),
        riprel_lowered: counter("riprel_lowered="),
        scratch_lowered: counter("scratch_lowered="),
        lea_lowered: counter("lea_lowered="),
        abs32_lowered: counter("abs32_lowered="),
        natural_lea_lowered: counter("natural_lea_lowered="),
        rip_indirect_lowered: counter("rip_indirect_lowered="),
        provenance_fallback: counter("provenance_fallback="),
        body_owner_recovered: counter("body_owner_recovered="),
        body_owner_published: counter("body_owner_published="),
        sse2_runs_admitted: counter("sse2_runs_admitted="),
        sse2_instructions_admitted: counter("sse2_instructions_admitted="),
        sse2_target_runs: counter("sse2_target_runs="),
        translations,
        line,
    }
}

#[test]
fn a_contiguous_sse2_run_matches_interpreter_and_native() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "sse2_chain");
    let (interpreted, interpreted_status, _) = run(&executable, "0");
    let (selected, selected_status, selected_backend) = run(&executable, "1");
    let native = std::process::Command::new(&executable)
        .output()
        .expect("native SSE2 fixture");
    assert_eq!(selected_status, interpreted_status);
    assert_eq!(selected, interpreted);
    assert_eq!(native.status.code(), Some(selected_status));
    assert_eq!(native.stdout, selected);
    assert_eq!(selected, b"sse2=00070005\n");
    assert!(selected_backend.sse2_runs_admitted > 0, "{}", selected_backend.line);
    assert!(
        selected_backend.sse2_instructions_admitted >= 5,
        "{}",
        selected_backend.line
    );
    assert_eq!(selected_backend.sse2_target_runs, 1, "{}", selected_backend.line);
}

#[test]
fn sse2_alignment_faults_replay_old_vectors_and_boundary_immediates_match_native() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "sse2_fault");
    let (interpreted, interpreted_status, _) = run(&executable, "0");
    let (selected, selected_status, selected_backend) = run_with_arguments(&executable, "1", &[], true, false);
    let native = std::process::Command::new(&executable)
        .output()
        .expect("native SSE2 fault fixture");
    assert_eq!(selected_status, interpreted_status);
    assert_eq!(selected, interpreted);
    assert_eq!(native.status.code(), Some(selected_status));
    assert_eq!(native.stdout, selected);
    assert!(
        selected_backend.sse2_instructions_admitted >= 8,
        "{}",
        selected_backend.line
    );
    assert!(selected_backend.provenance_fallback > 0, "{}", selected_backend.line);
    assert!(selected_backend.body_owner_recovered > 0, "{}", selected_backend.line);
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

/// Serialises the two tests which deliberately depend on whether the process-wide ELF link range is free.
static NONPIE_LINK_RANGE: Mutex<()> = Mutex::new(());

struct LinkPage {
    isa: u32,
    active: bool,
}

impl LinkPage {
    fn occupy(isa: u32) -> Self {
        hl_native::exec_page_cache_test(isa, 12).expect("occupy the ET_EXEC link page");
        Self { isa, active: true }
    }

    fn verify_and_release(mut self) {
        let result = hl_native::exec_page_cache_test(self.isa, 13);
        if result.is_ok() {
            self.active = false;
        }
        result.expect("verify and release the ET_EXEC link page");
    }
}

impl Drop for LinkPage {
    fn drop(&mut self) {
        if self.active {
            // A prior assertion may already be unwinding. Cleanup remains best-effort and must never
            // turn that first useful failure into a process abort from a second panic.
            if hl_native::exec_page_cache_test(self.isa, 13).is_ok() {
                self.active = false;
            }
        }
    }
}

#[test]
fn collision_guard_verification_is_explicit_and_drop_cannot_panic() {
    let source = include_str!("translit_differential.rs");
    let explicit_call = ["occupied", ".verify_and_release();"].concat();
    assert!(source.contains(&explicit_call));
    let drop_body = source
        .split_once("impl Drop for LinkPage")
        .and_then(|(_, tail)| tail.split_once("\n}\n\n#[test]\nfn collision_guard_verification"))
        .map(|(body, _)| body)
        .expect("LinkPage Drop body");
    for forbidden in [
        ".expect(",
        "unwrap(",
        "panic!(",
        "assert!(",
        "assert_eq!(",
        "assert_ne!(",
    ] {
        assert!(!drop_body.contains(forbidden), "LinkPage::drop contains {forbidden}");
    }
}

#[test]
fn collision_guard_drop_releases_during_unwind_and_target_state_is_local() {
    let _link_range = NONPIE_LINK_RANGE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for isa in [1, 2] {
        let other = if isa == 1 { 2 } else { 1 };
        let unwound = std::panic::catch_unwind(|| {
            let _occupied = LinkPage::occupy(isa);
            panic!("exercise collision cleanup during unwind");
        });
        assert!(unwound.is_err());
        let occupied = LinkPage::occupy(isa);
        assert_eq!(hl_native::exec_page_cache_test(other, 14), Err(-2));
        assert_eq!(hl_native::exec_page_cache_test(isa, 12), Err(-114));
        assert_eq!(hl_native::exec_page_cache_test(isa, 14), Err(-5));
        assert_eq!(hl_native::exec_page_cache_test(isa, 12), Err(-114));
        occupied.verify_and_release();
        assert_eq!(hl_native::exec_page_cache_test(isa, 13), Err(-2));
        assert_eq!(hl_native::exec_page_cache_test(other, 14), Err(-2));
    }
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
fn run_with_arguments(
    executable: &Path,
    translit: &str,
    extra: &[&[u8]],
    force_provenance_miss: bool,
    exhaust_body_owners: bool,
) -> (Vec<u8>, i32, Backend) {
    let captured = Arc::new(CapturedOutput::default());
    let mut options = Options::default();
    options.set("HL_TRANSLIT", translit, true).expect("HL_TRANSLIT");
    options.set("HL_C_DIAGNOSTICS", "1", true).expect("HL_C_DIAGNOSTICS");
    if force_provenance_miss {
        options
            .set("HL_TRANSLIT_PROVENANCE_FALLBACK", "1", true)
            .expect("HL_TRANSLIT_PROVENANCE_FALLBACK");
    }
    if exhaust_body_owners {
        options
            .set("HL_TRANSLIT_BODY_OWNER_EXHAUST", "1", true)
            .expect("HL_TRANSLIT_BODY_OWNER_EXHAUST");
    }
    let plan = RuntimePlan {
        rootfs: None,
        executable_host: Some(executable.as_os_str().as_encoded_bytes().to_vec()),
        arguments: std::iter::once(executable.as_os_str().as_encoded_bytes().to_vec())
            .chain(extra.iter().map(|argument| argument.to_vec()))
            .collect(),
        environment: Vec::new(),
        result_path: None,
        options,
        box_policy: Default::default(),
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

fn run(executable: &Path, translit: &str) -> (Vec<u8>, i32, Backend) {
    run_with_arguments(executable, translit, &[], false, false)
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

#[test]
fn exhausted_body_owner_capacity_falls_back_to_the_interpreter() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "flags");
    let (interpreted, interpreted_status, _) = run(&executable, "0");
    let (selected, selected_status, selected_backend) = run_with_arguments(&executable, "1", &[], false, true);
    assert_eq!(selected_status, interpreted_status);
    assert_eq!(selected, interpreted);
    assert_eq!(selected_backend.entries, 0, "{}", selected_backend.line);
    assert_eq!(selected_backend.body_owner_recovered, 0, "{}", selected_backend.line);
    assert_eq!(selected_backend.body_owner_published, 0, "{}", selected_backend.line);
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

/// Exact legacy SSE2 encodings, including high registers, mandatory-prefix
/// rejection, alignment faults and the register-only PSRLDQ group.
#[test]
fn exact_sse2_integer_forms_match_the_native_host() {
    agrees("sse2_exact");
}

/// A direct forward edge stays in one emitted descriptor. The corrupt bytes in the skipped gap make
/// target calculation observable, while the counters make a zero stitch budget observably red.
#[test]
fn a_same_page_forward_jump_is_stitched_without_executing_its_gap() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "forward_jump");
    let (interpreted, interpreted_status, interpreted_backend) = run(&executable, "0");
    let (selected, selected_status, selected_backend) = run(&executable, "1");
    assert_eq!(interpreted_backend.line, "[prof] translit: not selected");
    assert_eq!(interpreted_status, 0);
    assert_eq!(selected_status, interpreted_status);
    assert_eq!(selected, interpreted);
    assert_eq!(selected, b"42\n");
    assert!(selected_backend.stitch_candidates > 0, "{}", selected_backend.line);
    assert!(selected_backend.stitch_admitted > 0, "{}", selected_backend.line);
    assert!(
        selected_backend.stitch_admitted <= selected_backend.stitch_candidates,
        "{}",
        selected_backend.line
    );
}

#[test]
fn a_same_page_conditional_fallthrough_stays_in_the_descriptor() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "jcc_fallthrough");
    let (interpreted, interpreted_status, _) = run(&executable, "0");
    let (selected, selected_status, selected_backend) = run(&executable, "1");
    assert_eq!(interpreted_status, 0);
    assert_eq!(selected_status, interpreted_status);
    assert_eq!(selected, interpreted);
    assert_eq!(selected, b"fall=42 taken=41\n");
    assert!(selected_backend.jcc_fall_candidates > 0, "{}", selected_backend.line);
    assert!(selected_backend.jcc_fall_admitted > 0, "{}", selected_backend.line);
    assert!(selected_backend.jcc_fall_executed > 0, "{}", selected_backend.line);
    assert!(
        selected_backend.jcc_fall_admitted <= selected_backend.jcc_fall_candidates,
        "{}",
        selected_backend.line
    );
}

#[test]
fn a_cross_page_conditional_fallthrough_keeps_the_dispatch_boundary() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "jcc_page_boundary");
    let (interpreted, interpreted_status, _) = run(&executable, "0");
    let (selected, selected_status, selected_backend) = run(&executable, "1");
    assert_eq!(selected_status, interpreted_status);
    assert_eq!(selected, interpreted);
    assert_eq!(selected, b"fall=5 taken=6 straddle=5\n");
    assert!(selected_backend.jcc_fall_page_refused > 0, "{}", selected_backend.line);
    assert!(
        selected_backend.jcc_fall_successor_page_refused > 0,
        "{}",
        selected_backend.line
    );
}

#[test]
fn a_fault_after_an_internalized_fallthrough_keeps_guest_provenance() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "jcc_fallthrough_fault");
    let (interpreted, interpreted_status, _) = run(&executable, "0");
    let (selected, selected_status, selected_backend) = run(&executable, "1");
    assert_eq!(selected_status, interpreted_status);
    assert_eq!(selected, interpreted);
    assert_eq!(selected, b"fault=1 rip=1 r11=1 taken=7\n");
    assert!(selected_backend.jcc_fall_admitted > 0, "{}", selected_backend.line);
}

/// RIP-relative memory-indirect CALL/JMP, including a pointer load crossing into an unmapped page.
#[test]
fn rip_relative_indirect_control_preserves_answers_and_fault_state() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "rip_indirect");
    let (interpreted, interpreted_status, _) = run(&executable, "0");
    let (selected, selected_status, selected_backend) = run(&executable, "1");
    assert!(
        selected_backend.rip_indirect_lowered >= 3,
        "the fixture did not build its valid and page-boundary RIP-indirect terminators -- {}",
        selected_backend.line
    );
    assert_eq!(selected_status, interpreted_status);
    assert_eq!(selected, interpreted);
    let native = std::process::Command::new(&executable)
        .output()
        .expect("native fixture");
    assert_eq!(native.status.code(), Some(interpreted_status));
    assert_eq!(native.stdout, interpreted);

    // The interpreter currently changes rsp before reporting a failed CALL push. Keep the new emitted
    // path pinned to native architectural behaviour without making that older compatibility defect the
    // oracle for this lowering.
    let (selected_stack, selected_stack_status, selected_stack_backend) =
        run_with_arguments(&executable, "1", &[b"stack"], true, false);
    assert!(
        selected_stack_backend.rip_indirect_lowered >= 4,
        "{}",
        selected_stack_backend.line
    );
    assert_eq!(
        selected_stack_backend.provenance_fallback, 1,
        "{}",
        selected_stack_backend.line
    );
    assert_eq!(
        selected_stack_backend.body_owner_recovered, 1,
        "the forced instruction-ring miss did not recover through the immutable body owner -- {}",
        selected_stack_backend.line
    );
    let native_stack = std::process::Command::new(&executable)
        .arg("stack")
        .output()
        .expect("native stack-fault fixture");
    assert_eq!(selected_stack_status, 0, "{}", String::from_utf8_lossy(&selected_stack));
    assert_eq!(native_stack.status.code(), Some(0));
    assert_eq!(selected_stack, native_stack.stdout);
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

#[test]
fn a_non_position_independent_image_at_its_link_address_is_transliterated() {
    let _link_range = NONPIE_LINK_RANGE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let work = TempDir::new().unwrap();
    for name in [
        "flags",
        "operands",
        "sigs",
        "displaced_memory",
        "displaced_fault",
        "natural_abs32_fault",
        "natural_lea",
    ] {
        let executable = displaced_fixture(work.path(), name);
        let (interpreted, interpreted_status, _) = run(&executable, "0");
        let (selected, selected_status, selected_backend) = run(&executable, "1");
        assert_eq!(
            interpreted_status, 0,
            "{name}: the non-PIE fixture did not run under the interpreter"
        );
        assert!(
            selected_backend.entries > 0,
            "{name}: an ET_EXEC at its link address entered no emitted code -- {}",
            selected_backend.line
        );
        assert!(selected_backend.blocks > 0, "{name}: no transliterated block was built");
        assert_eq!(selected_backend.declined, 0, "{name}: a link-address image was refused");
        if name == "displaced_memory" || name == "displaced_fault" || name == "natural_abs32_fault" {
            assert!(
                selected_backend.riprel_lowered > 0,
                "{name}: a natural low ET_EXEC never entered absolute RIP-relative lowering -- {}",
                selected_backend.line
            );
            assert!(
                selected_backend.abs32_lowered > 0,
                "{name}: no abs32 load -- {}",
                selected_backend.line
            );
            assert_eq!(
                selected_backend.scratch_lowered, 0,
                "{name}: natural load borrowed a GPR"
            );
        }
        if name == "natural_lea" {
            assert!(
                selected_backend.natural_lea_lowered > 0,
                "natural LEA never entered immediate lowering -- {}",
                selected_backend.line
            );
            assert_eq!(selected_backend.scratch_lowered, 0, "natural LEA borrowed a GPR");
        }
        assert_eq!(
            selected_status, interpreted_status,
            "{name}: selecting the transliterator changed the exit status"
        );
        assert_eq!(
            String::from_utf8_lossy(&interpreted),
            String::from_utf8_lossy(&selected),
            "{name}: selecting the transliterator changed the output"
        );
        let native = std::process::Command::new(&executable)
            .output()
            .expect("native fixture");
        assert_eq!(native.status.code(), Some(interpreted_status));
        assert_eq!(native.stdout, interpreted, "{name}: engine output differs from native");
    }
}

/// An occupied link address must never be replaced. The loader instead uses displaced storage; stage-one
/// transliteration admits only memory-free and projected RIP-relative instructions and reports every
/// operand family that remains in the interpreter.
#[test]
fn an_occupied_nonpie_link_address_falls_back_without_clobbering() {
    let _link_range = NONPIE_LINK_RANGE.lock().unwrap();
    let occupied = LinkPage::occupy(2);
    let work = TempDir::new().unwrap();
    for name in [
        "flags",
        "operands",
        "sigs",
        "displaced_memory",
        "displaced_fault",
        "sse2_displaced_memory",
    ] {
        let executable = displaced_fixture(work.path(), name);
        let (interpreted, interpreted_status, _) = run(&executable, "0");
        let (selected, selected_status, selected_backend) = run(&executable, "1");
        assert!(
            selected_backend.line.contains("translit: displaced"),
            "{name}: the displaced image did not report its constrained backend -- {}",
            selected_backend.line
        );
        assert!(
            selected_backend.entries > 0,
            "{name}: displaced image entered no emitted code"
        );
        assert!(
            selected_backend.blocks > 0,
            "{name}: displaced image built no emitted code"
        );
        assert!(
            selected_backend.translations > 0,
            "{name}: interpreter translated no blocks"
        );
        assert!(
            selected_backend.declined > 0,
            "{name}: fixture reached no refused operand"
        );
        assert_eq!(selected_backend.operand_declined, selected_backend.declined);
        if name == "sse2_displaced_memory" {
            assert!(selected_backend.sse2_memory_declined > 0, "{}", selected_backend.line);
        }
        if name == "displaced_memory" || name == "displaced_fault" {
            assert!(
                selected_backend.riprel_lowered > 0,
                "the displaced accumulator dereference was not lowered -- {}",
                selected_backend.line
            );
            assert!(
                selected_backend.scratch_lowered > 0,
                "the displaced non-accumulator load was not lowered -- {}",
                selected_backend.line
            );
            assert!(
                selected_backend.lea_lowered > 0,
                "the displaced LEA was not lowered -- {}",
                selected_backend.line
            );
        }
        assert!(selected_backend.declined <= selected_backend.translations);
        assert_eq!(selected_status, interpreted_status, "{name}: exit status changed");
        assert_eq!(selected, interpreted, "{name}: output changed");
        let native = std::process::Command::new(&executable)
            .output()
            .expect("native fixture");
        assert_eq!(native.status.code(), Some(interpreted_status));
        assert_eq!(native.stdout, interpreted, "{name}: engine output differs from native");
    }
    occupied.verify_and_release();
}

/// Manual profile arm for a captured, real non-PIE tool. The caller supplies an owned root copy and
/// newline-delimited argv; keeping this ignored prevents a machine-local compiler corpus from becoming
/// a gate dependency. Reserving the link page in this process is the same collision seam as the exact
/// differential above, so a `translit: displaced` receipt is mandatory rather than inferred.
#[test]
#[ignore = "requires HL_PROFILE_CC1_ROOT and HL_PROFILE_CC1_ARGV"]
fn a_captured_cc1_runs_from_displaced_storage() {
    let root = PathBuf::from(std::env::var_os("HL_PROFILE_CC1_ROOT").expect("HL_PROFILE_CC1_ROOT"));
    let argv_path = PathBuf::from(std::env::var_os("HL_PROFILE_CC1_ARGV").expect("HL_PROFILE_CC1_ARGV"));
    let selected = std::env::var("HL_PROFILE_CC1_TRANSLIT").expect("HL_PROFILE_CC1_TRANSLIT");
    assert!(selected == "0" || selected == "1");
    let mut arguments: Vec<Vec<u8>> = std::fs::read(&argv_path)
        .expect("cc1 argv")
        .split(|byte| *byte == b'\n')
        .filter(|argument| !argument.is_empty())
        .map(<[u8]>::to_vec)
        .collect();
    let executable = arguments.first().expect("cc1 argv[0]").clone();
    let output = arguments
        .iter()
        .position(|argument| argument == b"-o")
        .and_then(|index| arguments.get_mut(index + 1))
        .expect("cc1 -o output");
    *output = b"/work/stage3-output.s".to_vec();
    let executable_host = root.join(
        Path::new(std::ffi::OsStr::from_bytes(&executable))
            .strip_prefix("/")
            .unwrap(),
    );
    let mut options = Options::default();
    options.set("HL_TRANSLIT", &selected, true).unwrap();
    options.set("HL_C_DIAGNOSTICS", "1", true).unwrap();
    let plan = RuntimePlan {
        rootfs: Some(root.as_os_str().as_encoded_bytes().to_vec()),
        executable_host: Some(executable_host.as_os_str().as_encoded_bytes().to_vec()),
        arguments,
        environment: vec![b"LC_ALL=C".to_vec()],
        result_path: None,
        options,
        box_policy: Default::default(),
    };
    let captured = Arc::new(CapturedOutput::default());
    let streams = StandardStreams::default().with_output(captured.clone());
    let _guard = NONPIE_LINK_RANGE.lock().unwrap();
    let occupied = LinkPage::occupy(2);
    let started = std::time::Instant::now();
    let engine = Engine::with_streams(GuestIsa::X86_64, plan, streams).expect("launch cc1");
    engine.start().expect("start cc1");
    let exit = engine.wait().expect("wait cc1");
    engine.destroy().expect("destroy cc1");
    let elapsed = started.elapsed();
    let report = backend(&captured.err.lock().unwrap());
    if selected == "1" {
        assert!(report.line.contains("translit: displaced"), "{}", report.line);
        assert!(report.blocks > 0 && report.entries > 0, "{}", report.line);
    } else {
        assert_eq!(report.line, "[prof] translit: not selected");
    }
    assert_eq!(exit.guest_status, 0, "{}", report.line);
    assert!(captured.out.lock().unwrap().is_empty());
    eprintln!(
        "[cc1-profile] selected={selected} elapsed_ns={} {}",
        elapsed.as_nanos(),
        report.line
    );
    occupied.verify_and_release();
}
