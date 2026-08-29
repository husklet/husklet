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
    jcc_link_admitted: u64,
    jcc_link_taken: u64,
    jcc_link_irq_fallback: u64,
    jcc_link_dispatcher: u64,
    direct_call_ibtc_emitted: u64,
    direct_call_ibtc_hits: u64,
    direct_call_ibtc_misses: u64,
    direct_call_ibtc_irq: u64,
    direct_call_ibtc_fills: u64,
    direct_call_ibtc_invalid_refusals: u64,
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
    body_owner_low_rotations: u64,
    body_owner_low_retranslations: u64,
    mixed_sse_encounters: u64,
    mixed_sse_admitted: u64,
    mixed_sse_transitions: u64,
    mixed_sse_executed: u64,
    mixed_sse_executed_transitions: u64,
    mixed_sse_disabled_boundaries: u64,
    sse2_runs_admitted: u64,
    sse2_instructions_admitted: u64,
    sse2_target_runs: u64,
    sse2_next_family_runs: u64,
    sse2_store_instructions: u64,
    sse2_store_movups: u64,
    sse2_store_movaps: u64,
    sse2_store_movdqu: u64,
    sse2_store_family_runs: u64,
    sse2_pxor_admitted: u64,
    sse2_pxor_runs_admitted: u64,
    sse2_punpcklqdq_admitted: u64,
    sse2_punpcklqdq_runs_admitted: u64,
    sse2_movd_admitted: u64,
    sse2_movd_runs_admitted: u64,
    sse2_movhlps_admitted: u64,
    fs_mem_admitted: u64,
    fs_fixture_admitted: u64,
    translations: u64,
    unsupported_total: u64,
    unsupported_keyed: u64,
    unsupported_overflow: u64,
    unsupported_sites: u64,
    unsupported_repeats: u64,
    unsupported_site_overflow: u64,
    translated_entries: u64,
    interpreted_entries: u64,
    translated_steps: u64,
    interpreted_steps: u64,
    root_pid: u64,
    claimed: u64,
    completed: u64,
    abnormal: u64,
    missing: u64,
    duplicate_finalize: u64,
    crossings: u64,
    reason_total: u64,
    shape_stitch_jmp: u64,
    shape_stitch_cond_fall: u64,
    shape_fallthrough: u64,
    shape_cond_taken: u64,
    shape_direct_jump: u64,
    shape_direct_call: u64,
    shape_jcc_taken_eligible: u64,
    shape_jcc_taken_chained: u64,
    shape_jcc_taken_dispatcher: u64,
    shape_fault: u64,
    shape_other: u64,
    family_jmem: u64,
    family_div_total: u64,
    family_div_inline: u64,
    family_div_service64: u64,
    family_div_service64_completed: u64,
    family_div_de: u64,
    family_idiv_total: u64,
    family_idiv_inline: u64,
    family_idiv_service64: u64,
    family_idiv_service64_completed: u64,
    family_idiv_de: u64,
    family_total: u64,
    would_link_candidates: u64,
    would_link_refusals: u64,
    would_fall_candidate: u64,
    would_jmp_candidate: u64,
    would_jmp_target_unmapped: u64,
    would_jmp_eligible: u64,
    would_call_candidate: u64,
    would_call_target_unmapped: u64,
    would_link_line: String,
    unsupported_line: String,
    tree_line: String,
    shape_line: String,
}

const MIXED_SSE_PROFILE_FIELDS: [&str; 6] = [
    "mixed_sse_encounters=",
    "mixed_sse_admitted=",
    "mixed_sse_transitions=",
    "mixed_sse_executed=",
    "mixed_sse_executed_transitions=",
    "mixed_sse_disabled_boundaries=",
];

fn exact_u64_field(line: &str, name: &str, context: &str) -> Result<u64, String> {
    let values = line
        .split_whitespace()
        .filter_map(|field| field.strip_prefix(name))
        .collect::<Vec<_>>();
    let [value] = values.as_slice() else {
        return Err(format!(
            "{context} field {name} appeared {} times, expected once in {line}",
            values.len()
        ));
    };
    value
        .parse::<u64>()
        .map_err(|_| format!("{context} field {name} is not a decimal integer in {line}"))
}

/// Parses the `[prof] translit: ...` line the exit report emits under `HL_C_DIAGNOSTICS`.
fn backend(stderr: &[u8]) -> Backend {
    let text = String::from_utf8_lossy(stderr);
    let lines = text
        .lines()
        .filter(|line| line.starts_with("[prof] translit:"))
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 1, "HL_C_DIAGNOSTICS produced:\n{text}");
    let line = lines[0].to_owned();
    let counter = |name: &str| {
        line.split_whitespace()
            .find_map(|field| field.strip_prefix(name))
            .and_then(|value| value.trim_end_matches(')').parse().ok())
            .unwrap_or(0)
    };
    let required_counter =
        |name: &str| exact_u64_field(&line, name, "typed translit").unwrap_or_else(|error| panic!("{error}"));
    // An unselected backend has no mixed-builder contract to prove. Every selected report form -- normal,
    // displaced, and store-alias-declined -- must carry the complete typed diagnostic surface.
    let mixed_selected = !line.contains("not selected") && !line.contains("absent");
    if mixed_selected {
        for name in MIXED_SSE_PROFILE_FIELDS {
            let _ = required_counter(name);
        }
    }
    let mixed_counter = |name: &str| {
        if !mixed_selected { 0 } else { required_counter(name) }
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
    let unsupported = text
        .lines()
        .find(|line| line.starts_with("[diag] unsupported "))
        .unwrap_or("");
    let unsupported_counter = |name: &str| {
        unsupported
            .split_whitespace()
            .find_map(|field| field.strip_prefix(name))
            .and_then(|value| value.parse().ok())
            .unwrap_or(0)
    };
    let trees = text
        .lines()
        .filter(|line| line.starts_with("[diag] backend-tree "))
        .collect::<Vec<_>>();
    assert_eq!(trees.len(), 1, "HL_C_DIAGNOSTICS produced:\n{text}");
    let tree = trees[0];
    let tree_counter = |name: &str| {
        tree.split_whitespace()
            .find_map(|field| field.strip_prefix(name))
            .and_then(|value| value.parse().ok())
            .unwrap_or(0)
    };
    let reason_total = (0..16)
        .map(|reason| tree_counter(&format!("reason{reason}=")))
        .sum::<u64>()
        + tree_counter("reason_other=");
    let shapes = text
        .lines()
        .filter(|line| line.starts_with("[diag] backend-shape "))
        .collect::<Vec<_>>();
    assert_eq!(shapes.len(), 1, "HL_C_DIAGNOSTICS produced:\n{text}");
    let shape = shapes[0];
    let shape_counter = |name: &str| {
        shape
            .split_whitespace()
            .find_map(|field| field.strip_prefix(name))
            .and_then(|value| value.parse().ok())
            .unwrap_or(0)
    };
    let shape_required_counter =
        |name: &str| exact_u64_field(shape, name, "typed backend-shape").unwrap_or_else(|error| panic!("{error}"));
    if mixed_selected {
        for name in [
            "mixed_sse_executed=",
            "mixed_sse_executed_transitions=",
            "mixed_sse_disabled_boundaries=",
        ] {
            assert!(
                shape_required_counter(name) >= required_counter(name),
                "fork-shared {name} regressed below root-local telemetry: {shape}\n{line}"
            );
        }
    }
    let family_counter = |name: &str| {
        shape
            .split_whitespace()
            .find_map(|field| field.strip_prefix(name))
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or_else(|| panic!("missing typed executed-family field {name} in {shape}"))
    };
    let would_links = text
        .lines()
        .filter(|line| line.starts_with("[diag] backend-would-link "))
        .collect::<Vec<_>>();
    assert_eq!(would_links.len(), 1, "HL_C_DIAGNOSTICS produced:\n{text}");
    let would_link = would_links[0];
    assert!(
        would_link.split_whitespace().any(|field| field == "version=1"),
        "{would_link}"
    );
    let would_link_counter = |name: &str| {
        would_link
            .split_whitespace()
            .find_map(|field| field.strip_prefix(name))
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or_else(|| panic!("missing typed would-link field {name} in {would_link}"))
    };
    for family in ["fall", "jmp", "call"] {
        let refusals = [
            "source_unresolved",
            "cross_page",
            "target_unmapped",
            "target_untranslated",
            "generation",
            "target_page",
            "rel32",
        ]
        .into_iter()
        .map(|reason| would_link_counter(&format!("{family}_{reason}=")))
        .sum::<u64>();
        assert_eq!(
            would_link_counter(&format!("{family}_candidate=")),
            would_link_counter(&format!("{family}_eligible=")) + refusals,
            "{would_link}"
        );
    }
    assert_eq!(
        would_link_counter("fall_candidate="),
        shape_counter("t_fallthrough="),
        "executed fallthrough terminals must reconcile exactly: {would_link}\n{shape}"
    );
    assert_eq!(
        would_link_counter("jmp_candidate="),
        shape_counter("t_direct_jump="),
        "executed direct-JMP terminals must reconcile exactly: {would_link}\n{shape}"
    );
    assert_eq!(
        would_link_counter("call_candidate="),
        shape_counter("t_direct_call="),
        "executed direct-CALL terminals must reconcile exactly: {would_link}\n{shape}"
    );
    let translated_exit_total = [
        "t_fallthrough=",
        "t_cond_taken=",
        "t_cond_not_taken=",
        "t_direct_jump=",
        "t_direct_call=",
        "t_return=",
        "t_indirect_branch=",
        "t_indirect_call=",
        "t_syscall=",
        "t_irq=",
        "t_fault=",
        "t_other=",
    ]
    .into_iter()
    .map(shape_counter)
    .sum::<u64>();
    assert_eq!(translated_exit_total, tree_counter("translated_entries="), "{shape}");
    assert_eq!(
        shape_counter("translated_transfers="),
        tree_counter("translated_entries=")
            + shape_counter("stitch_jmp=")
            + shape_counter("stitch_cond_fall=")
            + ["fall", "jt", "jn", "jmp", "call"]
                .into_iter()
                .map(|family| shape_counter(&format!("e_{family}_chained=")))
                .sum::<u64>(),
        "{shape}"
    );
    for family in ["fall", "jt", "jn", "jmp", "call"] {
        let total = shape_counter(&format!("e_{family}_total="));
        assert_eq!(
            shape_counter(&format!("e_{family}_mapped="))
                + shape_counter(&format!("e_{family}_unmapped="))
                + shape_counter(&format!("e_{family}_interrupted=")),
            total,
            "{shape}"
        );
        assert_eq!(
            shape_counter(&format!("e_{family}_chained=")) + shape_counter(&format!("e_{family}_dispatcher=")),
            total,
            "{shape}"
        );
    }
    assert_eq!(
        shape_counter("jt_same_page=") + shape_counter("jt_cross_page="),
        shape_counter("e_jt_total="),
        "{shape}"
    );
    assert_eq!(
        shape_counter("jt_target_translated=") + shape_counter("jt_target_interpreted="),
        shape_counter("e_jt_mapped="),
        "{shape}"
    );
    assert_eq!(
        shape_counter("jt_generation_current=") + shape_counter("jt_generation_retired="),
        shape_counter("jt_target_translated="),
        "{shape}"
    );
    assert_eq!(
        shape_counter("jt_rel32=") + shape_counter("jt_rel32_unreachable="),
        shape_counter("jt_target_translated="),
        "{shape}"
    );
    assert_eq!(
        shape_counter("jt_eligible=") + shape_counter("jt_ineligible=") + shape_counter("e_jt_interrupted="),
        shape_counter("e_jt_total="),
        "{shape}"
    );
    assert_eq!(
        family_counter("family_div_total="),
        family_counter("family_div_inline=")
            + family_counter("family_div_service64=")
            + family_counter("family_div_de="),
        "{shape}"
    );
    assert_eq!(
        family_counter("family_idiv_total="),
        family_counter("family_idiv_inline=")
            + family_counter("family_idiv_service64=")
            + family_counter("family_idiv_de="),
        "{shape}"
    );
    assert!(
        family_counter("family_div_service64_completed=") <= family_counter("family_div_service64="),
        "{shape}"
    );
    assert!(
        family_counter("family_idiv_service64_completed=") <= family_counter("family_idiv_service64="),
        "{shape}"
    );
    assert_eq!(
        family_counter("family_total="),
        family_counter("family_jmem=") + family_counter("family_div_total=") + family_counter("family_idiv_total="),
        "{shape}"
    );
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
        jcc_link_admitted: counter("jcc_link_admitted="),
        jcc_link_taken: counter("jcc_link_taken="),
        jcc_link_irq_fallback: counter("jcc_link_irq_fallback="),
        jcc_link_dispatcher: counter("jcc_link_dispatcher="),
        direct_call_ibtc_emitted: shape_counter("direct_call_ibtc_emitted="),
        direct_call_ibtc_hits: shape_counter("direct_call_ibtc_hits="),
        direct_call_ibtc_misses: shape_counter("direct_call_ibtc_misses="),
        direct_call_ibtc_irq: shape_counter("direct_call_ibtc_irq="),
        direct_call_ibtc_fills: shape_counter("direct_call_ibtc_fills="),
        direct_call_ibtc_invalid_refusals: shape_counter("direct_call_ibtc_invalid_refusals="),
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
        body_owner_low_rotations: counter("body_owner_low_rotations="),
        body_owner_low_retranslations: counter("body_owner_low_retranslations="),
        mixed_sse_encounters: mixed_counter("mixed_sse_encounters="),
        mixed_sse_admitted: mixed_counter("mixed_sse_admitted="),
        mixed_sse_transitions: mixed_counter("mixed_sse_transitions="),
        // Publication telemetry is process-local; completed execution is authoritative only in the
        // exactly-one fork-shared backend-shape record emitted after the complete process tree reaps.
        mixed_sse_executed: shape_required_counter("mixed_sse_executed="),
        mixed_sse_executed_transitions: shape_required_counter("mixed_sse_executed_transitions="),
        mixed_sse_disabled_boundaries: shape_required_counter("mixed_sse_disabled_boundaries="),
        sse2_runs_admitted: counter("sse2_runs_admitted="),
        sse2_instructions_admitted: counter("sse2_instructions_admitted="),
        sse2_target_runs: counter("sse2_target_runs="),
        sse2_next_family_runs: counter("sse2_next_family_runs="),
        sse2_store_instructions: counter("sse2_store_instructions="),
        sse2_store_movups: counter("sse2_store_movups="),
        sse2_store_movaps: counter("sse2_store_movaps="),
        sse2_store_movdqu: counter("sse2_store_movdqu="),
        sse2_store_family_runs: counter("sse2_store_family_runs="),
        sse2_pxor_admitted: counter("sse2_pxor_admitted="),
        sse2_pxor_runs_admitted: counter("sse2_pxor_runs_admitted="),
        sse2_punpcklqdq_admitted: counter("sse2_punpcklqdq_admitted="),
        sse2_punpcklqdq_runs_admitted: counter("sse2_punpcklqdq_runs_admitted="),
        sse2_movd_admitted: counter("sse2_movd_admitted="),
        sse2_movd_runs_admitted: counter("sse2_movd_runs_admitted="),
        sse2_movhlps_admitted: counter("sse2_movhlps_admitted="),
        fs_mem_admitted: counter("fs_mem_admitted="),
        fs_fixture_admitted: counter("fs_fixture_admitted="),
        translations,
        unsupported_total: unsupported_counter("total="),
        unsupported_keyed: unsupported_counter("keyed="),
        unsupported_overflow: unsupported_counter("overflow="),
        unsupported_sites: unsupported_counter("sites="),
        unsupported_repeats: unsupported_counter("repeats="),
        unsupported_site_overflow: unsupported_counter("site_overflow="),
        translated_entries: tree_counter("translated_entries="),
        interpreted_entries: tree_counter("interpreted_entries="),
        translated_steps: tree_counter("translated_steps="),
        interpreted_steps: tree_counter("interpreted_steps="),
        root_pid: tree_counter("root_pid="),
        claimed: tree_counter("claimed="),
        completed: tree_counter("completed="),
        abnormal: tree_counter("abnormal="),
        missing: tree_counter("missing="),
        duplicate_finalize: tree_counter("duplicate_finalize="),
        crossings: tree_counter("crossings="),
        reason_total,
        shape_stitch_jmp: shape_counter("stitch_jmp="),
        shape_stitch_cond_fall: shape_counter("stitch_cond_fall="),
        shape_fallthrough: shape_counter("t_fallthrough="),
        shape_cond_taken: shape_counter("t_cond_taken="),
        shape_direct_jump: shape_counter("t_direct_jump="),
        shape_direct_call: shape_counter("t_direct_call="),
        shape_jcc_taken_eligible: shape_counter("jt_eligible="),
        shape_jcc_taken_chained: shape_counter("e_jt_chained="),
        shape_jcc_taken_dispatcher: shape_counter("e_jt_dispatcher="),
        shape_fault: shape_counter("t_fault="),
        shape_other: shape_counter("t_other="),
        family_jmem: family_counter("family_jmem="),
        family_div_total: family_counter("family_div_total="),
        family_div_inline: family_counter("family_div_inline="),
        family_div_service64: family_counter("family_div_service64="),
        family_div_service64_completed: family_counter("family_div_service64_completed="),
        family_div_de: family_counter("family_div_de="),
        family_idiv_total: family_counter("family_idiv_total="),
        family_idiv_inline: family_counter("family_idiv_inline="),
        family_idiv_service64: family_counter("family_idiv_service64="),
        family_idiv_service64_completed: family_counter("family_idiv_service64_completed="),
        family_idiv_de: family_counter("family_idiv_de="),
        family_total: family_counter("family_total="),
        would_link_candidates: ["fall", "jmp", "call"]
            .into_iter()
            .map(|family| would_link_counter(&format!("{family}_candidate=")))
            .sum(),
        would_link_refusals: ["fall", "jmp", "call"]
            .into_iter()
            .map(|family| {
                would_link_counter(&format!("{family}_candidate=")) - would_link_counter(&format!("{family}_eligible="))
            })
            .sum(),
        would_fall_candidate: would_link_counter("fall_candidate="),
        would_jmp_candidate: would_link_counter("jmp_candidate="),
        would_jmp_target_unmapped: would_link_counter("jmp_target_unmapped="),
        would_jmp_eligible: would_link_counter("jmp_eligible="),
        would_call_candidate: would_link_counter("call_candidate="),
        would_call_target_unmapped: would_link_counter("call_target_unmapped="),
        would_link_line: would_link.to_owned(),
        unsupported_line: unsupported.to_owned(),
        tree_line: tree.to_owned(),
        shape_line: shape.to_owned(),
        line,
    }
}

#[test]
fn unsupported_census_records_successful_interpreter_steps() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "unsupported_census");
    let (interpreted, status, report) = run(&executable, "0");
    let (selected, selected_status, selected_report) = run(&executable, "1");
    let native = std::process::Command::new(&executable)
        .output()
        .expect("native unsupported-census fixture");
    assert_eq!(status, 0);
    assert_eq!(native.status.code(), Some(0));
    assert_eq!(interpreted, native.stdout);
    assert_eq!((selected_status, selected), (status, interpreted.clone()));
    assert!(report.unsupported_total > 0, "{}", report.line);
    assert!(
        selected_report.translated_entries > 0 && selected_report.interpreted_entries > 0,
        "{}; {}",
        selected_report.line,
        selected_report.tree_line,
    );
    assert!(selected_report.translated_steps >= selected_report.translated_entries);
    assert!(
        selected_report.would_link_candidates > 0,
        "{}",
        selected_report.would_link_line
    );
    assert!(
        selected_report.would_link_refusals > 0,
        "{}",
        selected_report.would_link_line
    );
    assert!(report.interpreted_steps >= report.interpreted_entries);
    assert_eq!(
        report.unsupported_total,
        report.unsupported_keyed + report.unsupported_overflow
    );
    assert_eq!(
        report.unsupported_total,
        report.unsupported_sites + report.unsupported_repeats + report.unsupported_site_overflow
    );
    assert!(
        report.unsupported_line.contains("9830009d4:128"),
        "{}",
        report.unsupported_line
    );
}

#[test]
fn executed_jmem_div_and_idiv_families_have_dedicated_completed_counts() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "executed_families");
    let (interpreted, interpreted_status, _) = run(&executable, "0");
    let (selected, selected_status, report) = run(&executable, "1");
    let native = std::process::Command::new(&executable)
        .output()
        .expect("native executed-family fixture");
    assert_eq!(interpreted_status, 0);
    assert_eq!(selected_status, interpreted_status);
    assert_eq!(native.status.code(), Some(0));
    assert_eq!(selected, interpreted);
    assert_eq!(selected, native.stdout);
    assert_eq!(report.family_jmem, 1, "{}", report.shape_line);
    assert_eq!(report.family_div_inline, 1, "{}", report.shape_line);
    assert!(report.family_div_service64 >= 1, "{}", report.shape_line);
    assert_eq!(report.family_div_de, 1, "{}", report.shape_line);
    assert_eq!(report.family_idiv_inline, 1, "{}", report.shape_line);
    assert!(report.family_idiv_service64 >= 1, "{}", report.shape_line);
    assert_eq!(report.family_idiv_de, 1, "{}", report.shape_line);
    assert_eq!(
        report.family_div_service64_completed, report.family_div_service64,
        "{}",
        report.shape_line
    );
    assert_eq!(
        report.family_idiv_service64_completed, report.family_idiv_service64,
        "{}",
        report.shape_line
    );
    assert_eq!(
        report.family_total,
        report.family_jmem + report.family_div_total + report.family_idiv_total,
        "{}",
        report.shape_line
    );
}

#[test]
fn memory_indirect_jump_addressing_forms_match_interpreter_and_native() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "jmp_mem_family");
    let (interpreted, interpreted_status, interpreted_report) = run(&executable, "0");
    let (selected, selected_status, report) = run_with_jcc_ibtc_disabled(&executable);
    let native = std::process::Command::new(&executable)
        .output()
        .expect("native FF /4 fixture");
    assert_eq!(interpreted_status, 0);
    assert_eq!(selected_status, interpreted_status);
    assert_eq!(selected, interpreted);
    assert_eq!(native.status.code(), Some(0));
    assert_eq!(native.stdout, selected);
    assert_eq!(selected, b"base=1 index=1 rsp=1 r12=1 rbp=1 r13=1\n");
    assert!(interpreted_report.family_jmem >= 6, "{}", interpreted_report.shape_line);
    assert_eq!(report.family_jmem, 0, "{}", report.shape_line);
}

#[test]
fn memory_indirect_jump_faults_preserve_source_state() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "jmp_mem_fault");
    let (interpreted, interpreted_status, _) = run(&executable, "0");
    let (selected, selected_status, report) = run_with_jcc_ibtc_disabled(&executable);
    assert_eq!(interpreted_status, 0, "{}", String::from_utf8_lossy(&interpreted));
    assert_eq!(selected_status, interpreted_status);
    assert_eq!(selected, interpreted);
    assert_eq!(
        selected,
        b"faults=4 mismatch=0 unmapped=1 protected=1 split=1 noncanonical=1\n"
    );
    assert_eq!(report.family_jmem, 0, "{}", report.shape_line);
}

#[test]
fn memory_indirect_jump_survives_real_smc_and_fork() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "jmp_mem_smc_fork");
    let (interpreted, interpreted_status, _) = run(&executable, "0");
    let (selected, selected_status, _) = run_with_jcc_ibtc_disabled(&executable);
    assert_eq!(interpreted_status, 0, "{}", String::from_utf8_lossy(&interpreted));
    assert_eq!(
        selected_status,
        interpreted_status,
        "{}",
        String::from_utf8_lossy(&selected)
    );
    assert_eq!(selected, interpreted);
    assert_eq!(selected, b"before=1 smc=1 fork=1\n");
}

#[test]
fn register_movhlps_matches_native_for_distinct_high_alias_and_flags() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "sse_movhlps");
    let (interpreted, interpreted_status, _) = run(&executable, "0");
    let (selected, selected_status, selected_backend) = run(&executable, "1");
    let native = std::process::Command::new(&executable)
        .output()
        .expect("native MOVHLPS fixture");
    assert_eq!(selected_status, interpreted_status);
    assert_eq!(selected, interpreted);
    assert_eq!(native.status.code(), Some(selected_status));
    assert_eq!(native.stdout, selected);
    assert_eq!(
        selected,
        b"distinct=30dfcefdec9b8ab9:10ffeeddccbbaa99 high=30dfcefdec9b8ab9:10ffeeddccbbaa99 \
alias=10ffeeddccbbaa99:10ffeeddccbbaa99 flags=0c95:0c95:0000\n"
    );
    assert_eq!(selected_backend.sse2_movhlps_admitted, 3, "{}", selected_backend.line);
}

#[test]
fn fs_disp32_mov_and_sub_match_interpreter_and_native() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "fs_tls");
    let (interpreted, interpreted_status, _) = run(&executable, "0");
    let (selected, selected_status, selected_backend) = run(&executable, "1");
    let native = std::process::Command::new(&executable)
        .output()
        .expect("native FS fixture");
    assert_eq!((selected_status, &selected), (interpreted_status, &interpreted));
    assert_eq!(native.status.code(), Some(selected_status));
    assert_eq!(native.stdout, selected);
    assert_eq!(
        selected,
        b"fs rc=0 mov=0123456789abcdef high=f0e1d2c3b4a59687 sub=edcba987654320ff flags=0094 r11=13579bdf2468ace0 r10=1020304050607080 neg=55aa33cc77ee11dd threads=1 authority=0\n"
    );
    assert!(selected_backend.fs_mem_admitted >= 3, "{}", selected_backend.line);
    assert_eq!(selected_backend.fs_fixture_admitted, 5, "{}", selected_backend.line);
    assert!(selected_backend.entries >= 3, "{}", selected_backend.line);
}

#[test]
fn fs_disp32_loads_fall_back_under_direct_data_authority() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "fs_tls");
    let args = [b"direct".as_slice()];
    let (interpreted, interpreted_status, _) = run_with_arguments(&executable, "0", &args, false, false, true, false);
    let (selected, selected_status, selected_backend) =
        run_with_arguments(&executable, "1", &args, false, false, true, false);
    let native = std::process::Command::new(&executable)
        .arg("direct")
        .output()
        .expect("native FS authority fixture");
    assert_eq!((selected_status, &selected), (interpreted_status, &interpreted));
    assert_eq!(native.status.code(), Some(selected_status));
    assert_eq!(native.stdout, selected);
    assert!(selected.ends_with(b"threads=1 authority=0\n"));
    assert_eq!(selected_backend.fs_fixture_admitted, 0, "{}", selected_backend.line);
}

#[test]
fn fs_disp32_fault_restarts_at_guest_source_with_old_destination() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "fs_tls_fault");
    let (interpreted, interpreted_status, _) = run(&executable, "0");
    let (selected, selected_status, selected_backend) =
        run_with_arguments(&executable, "1", &[], true, false, false, false);
    let native = std::process::Command::new(&executable)
        .output()
        .expect("native FS fault fixture");
    assert_eq!((selected_status, &selected), (interpreted_status, &interpreted));
    assert_eq!(native.status.code(), Some(selected_status));
    assert_eq!(native.stdout, selected);
    assert_eq!(
        selected,
        b"fs-fault delivered=1 rip=4 destination=4 scratch=4 flags=2 result=8877665544332211 high=33445566778899aa sub=1122334455667788 subhigh=33445566778899aa\n"
    );
    assert!(selected_backend.fs_mem_admitted > 0, "{}", selected_backend.line);
    assert!(selected_backend.provenance_fallback > 0, "{}", selected_backend.line);
    assert!(selected_backend.body_owner_recovered > 0, "{}", selected_backend.line);
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
        selected_backend.sse2_instructions_admitted >= 6,
        "{}",
        selected_backend.line
    );
    assert_eq!(selected_backend.sse2_target_runs, 1, "{}", selected_backend.line);
}

#[test]
fn mixed_sse_profile_fields_are_single_decimal_tokens() {
    let good = MIXED_SSE_PROFILE_FIELDS
        .iter()
        .map(|name| format!("{name}7"))
        .collect::<Vec<_>>()
        .join(" ");
    for name in MIXED_SSE_PROFILE_FIELDS {
        assert_eq!(exact_u64_field(&good, name, "typed translit").unwrap(), 7);
        let missing = good.replace(&format!("{name}7"), "");
        assert!(
            exact_u64_field(&missing, name, "typed translit")
                .unwrap_err()
                .contains("appeared 0 times")
        );
        let duplicate = format!("{good} {name}8");
        assert!(
            exact_u64_field(&duplicate, name, "typed translit")
                .unwrap_err()
                .contains("appeared 2 times")
        );
        let nondecimal = good.replace(&format!("{name}7"), &format!("{name}seven"));
        assert!(
            exact_u64_field(&nondecimal, name, "typed translit")
                .unwrap_err()
                .contains("not a decimal")
        );
    }
}

#[test]
fn body_owner_low_watermark_rotates_through_the_single_thread_dispatcher() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "mixed_sse");
    let (interpreted, interpreted_status, _) = run(&executable, "0");
    let (selected, selected_status, report) = run_with_body_owner_rotation(&executable);
    assert_eq!((selected_status, &selected), (interpreted_status, &interpreted));
    assert_eq!(report.body_owner_low_rotations, 2, "{}", report.line);
    assert!(report.body_owner_low_retranslations >= 2, "{}", report.line);
    assert!(report.mixed_sse_admitted > 0, "{}", report.line);
    assert!(report.mixed_sse_executed > 0, "{}", report.shape_line);
}

#[test]
fn alternating_normal_and_sse_share_one_exact_recovery_descriptor() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "mixed_sse");
    let (interpreted, interpreted_status, _) = run(&executable, "0");
    let (selected, selected_status, selected_backend) =
        run_with_arguments(&executable, "1", &[], true, false, false, false);
    let (disabled, disabled_status, disabled_backend) =
        run_with_arguments(&executable, "1", &[], true, false, false, true);
    let native = std::process::Command::new(&executable)
        .output()
        .expect("native mixed normal/SSE fixture");
    assert_eq!((selected_status, &selected), (interpreted_status, &interpreted));
    assert_eq!((disabled_status, &disabled), (interpreted_status, &interpreted));
    assert_eq!(native.status.code(), Some(selected_status));
    assert_eq!(native.stdout, selected);
    assert_eq!(
        selected,
        b"mixed state=1 faults=7 registers=7 vectors=04030201/04030201/04030201 fork=1\n"
    );
    assert!(selected_backend.mixed_sse_encounters > 0, "{}", selected_backend.line);
    assert!(selected_backend.mixed_sse_admitted > 0, "{}", selected_backend.line);
    assert!(
        selected_backend.mixed_sse_transitions >= selected_backend.mixed_sse_admitted,
        "{}",
        selected_backend.line
    );
    assert!(selected_backend.body_owner_recovered >= 3, "{}", selected_backend.line);
    assert!(disabled_backend.mixed_sse_encounters > 0, "{}", disabled_backend.line);
    assert_eq!(disabled_backend.mixed_sse_admitted, 0, "{}", disabled_backend.line);
    assert_eq!(disabled_backend.mixed_sse_transitions, 0, "{}", disabled_backend.line);
    assert!(selected_backend.mixed_sse_executed > 0, "{}", selected_backend.line);
    assert!(
        selected_backend.mixed_sse_executed_transitions >= selected_backend.mixed_sse_executed,
        "{}",
        selected_backend.line
    );
    assert_eq!(
        selected_backend.mixed_sse_disabled_boundaries, 0,
        "{}",
        selected_backend.line
    );
    assert_eq!(disabled_backend.mixed_sse_executed, 0, "{}", disabled_backend.line);
    assert_eq!(
        disabled_backend.mixed_sse_executed_transitions, 0,
        "{}",
        disabled_backend.line
    );
    assert!(
        disabled_backend.mixed_sse_disabled_boundaries > 0,
        "{}",
        disabled_backend.line
    );
}

#[test]
fn signal_ucontext_round_trips_pf_and_af() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "signal_pf_af");
    let (interpreted, interpreted_status, _) = run(&executable, "0");
    let (selected, selected_status, selected_backend) = run(&executable, "1");
    let native = std::process::Command::new(&executable)
        .output()
        .expect("native PF/AF signal-ucontext fixture");
    assert_eq!((selected_status, &selected), (interpreted_status, &interpreted));
    assert_eq!(native.status.code(), Some(selected_status));
    assert_eq!(native.stdout, selected);
    assert_eq!(selected, b"pf-af frame=04/10 resumed=10/04\n");
    assert!(selected_backend.entries > 0, "{}", selected_backend.line);
}

#[test]
fn signal_ucontext_projects_but_does_not_restore_if_and_id() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "signal_if_id");
    let (interpreted, interpreted_status, _) = run(&executable, "0");
    let (selected, selected_status, selected_backend) = run(&executable, "1");
    let native = std::process::Command::new(&executable)
        .output()
        .expect("native IF/ID signal-ucontext fixture");
    assert_eq!((selected_status, &selected), (interpreted_status, &interpreted));
    assert_eq!(native.status.code(), Some(selected_status));
    assert_eq!(native.stdout, selected);
    assert_eq!(selected, b"if-id frame=200200/000200 resumed=200200/200200\n");
    assert!(selected_backend.entries > 0, "{}", selected_backend.line);
}

#[test]
fn register_pxor_matches_native_for_distinct_high_alias_and_flags() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "sse_pxor");
    let (interpreted, interpreted_status, _) = run(&executable, "0");
    let (selected, selected_status, selected_backend) = run(&executable, "1");
    let native = std::process::Command::new(&executable)
        .output()
        .expect("native PXOR fixture");
    assert_eq!((selected_status, &selected), (interpreted_status, &interpreted));
    assert_eq!(native.status.code(), Some(selected_status));
    assert_eq!(native.stdout, selected);
    assert_eq!(
        selected,
        b"distinct=1010101010101010:2020202020202020 high=ffffffffffffffff:aaaaaaaaaaaaaaaa zero=0000000000000000:0000000000000000 flags=0c95:0c95:0000\n"
    );
    assert!(selected_backend.sse2_pxor_admitted >= 3, "{}", selected_backend.line);
    assert!(
        selected_backend.sse2_pxor_runs_admitted > 0,
        "{}",
        selected_backend.line
    );
    let image = std::fs::read(executable).unwrap();
    assert!(image.windows(4).any(|bytes| bytes == [0x66, 0x0f, 0xef, 0xc1]));
    assert!(image.windows(5).any(|bytes| bytes == [0x66, 0x45, 0x0f, 0xef, 0xc1]));
}

#[test]
fn register_punpcklqdq_matches_native_for_distinct_high_alias_and_flags() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "sse_punpcklqdq");
    let (interpreted, interpreted_status, _) = run(&executable, "0");
    let (selected, selected_status, selected_backend) = run(&executable, "1");
    let native = std::process::Command::new(&executable)
        .output()
        .expect("native PUNPCKLQDQ fixture");
    assert_eq!((selected_status, &selected), (interpreted_status, &interpreted));
    assert_eq!(native.status.code(), Some(selected_status));
    assert_eq!(native.stdout, selected);
    assert_eq!(
        selected,
        b"distinct=8877665544332211:0123456789abcdef high=0706050403020100:f8f9fafbfcfdfeff alias=8877665544332211:8877665544332211 flags=0c95:0c95:0000\n"
    );
    assert!(
        selected_backend.sse2_punpcklqdq_admitted >= 3,
        "{}",
        selected_backend.line
    );
    assert!(
        selected_backend.sse2_punpcklqdq_runs_admitted > 0,
        "{}",
        selected_backend.line
    );
    let image = std::fs::read(executable).unwrap();
    assert!(image.windows(4).any(|bytes| bytes == [0x66, 0x0f, 0x6c, 0xc1]));
    assert!(image.windows(5).any(|bytes| bytes == [0x66, 0x45, 0x0f, 0x6c, 0xc1]));
}

#[test]
fn register_movd_movq_matches_native_for_width_high_registers_and_flags() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "sse_movd");
    let (interpreted, interpreted_status, _) = run(&executable, "0");
    let (selected, selected_status, selected_backend) = run(&executable, "1");
    let native = std::process::Command::new(&executable)
        .output()
        .expect("native MOVD/MOVQ fixture");
    assert_eq!((selected_status, &selected), (interpreted_status, &interpreted));
    assert_eq!(native.status.code(), Some(selected_status));
    assert_eq!(native.stdout, selected);
    assert_eq!(
        selected,
        b"d32=0000000055667788:0000000000000000 q64=1122334455667788:0000000000000000 high32=00000000ccddeeff:0000000000000000 high64=8899aabbccddeeff:0000000000000000 flags=0c95:0c95:0000\n"
    );
    assert!(selected_backend.sse2_movd_admitted >= 4, "{}", selected_backend.line);
    assert!(
        selected_backend.sse2_movd_runs_admitted > 0,
        "{}",
        selected_backend.line
    );
    let image = std::fs::read(executable).unwrap();
    let contains = |bytes: &[u8]| image.windows(bytes.len()).any(|window| window == bytes);
    assert!(contains(&[0x66, 0x0f, 0x6e, 0xc0]));
    assert!(contains(&[0x66, 0x48, 0x0f, 0x6e, 0xc8]));
    assert!(contains(&[0x66, 0x45, 0x0f, 0x6e, 0xd1]));
    assert!(contains(&[0x66, 0x4d, 0x0f, 0x6e, 0xd9]));
}

#[test]
fn movd_movq_body_owner_exhaustion_falls_back_without_partial_authority() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "sse_movd");
    let (interpreted, interpreted_status, _) = run(&executable, "0");
    let (selected, selected_status, selected_backend) =
        run_with_arguments(&executable, "1", &[], false, true, false, false);
    assert_eq!((selected_status, selected), (interpreted_status, interpreted));
    assert_eq!(selected_backend.entries, 0, "{}", selected_backend.line);
    assert_eq!(selected_backend.sse2_movd_admitted, 0, "{}", selected_backend.line);
    assert_eq!(selected_backend.sse2_movd_runs_admitted, 0, "{}", selected_backend.line);
}

#[test]
fn sse2_alignment_faults_replay_old_vectors_and_boundary_immediates_match_native() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "sse2_fault");
    let (interpreted, interpreted_status, _) = run(&executable, "0");
    let (selected, selected_status, selected_backend) =
        run_with_arguments(&executable, "1", &[], true, false, false, false);
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
fn run_with_arguments_internal(
    executable: &Path,
    translit: &str,
    extra: &[&[u8]],
    force_provenance_miss: bool,
    exhaust_body_owners: bool,
    force_fs_authority: bool,
    disable_mixed_sse: bool,
    force_body_owner_rotation: bool,
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
    if force_fs_authority {
        options
            .set("HL_TRANSLIT_FS_AUTHORITY_TEST", "1", true)
            .expect("HL_TRANSLIT_FS_AUTHORITY_TEST");
    }
    if disable_mixed_sse {
        options
            .set("HL_TRANSLIT_MIXED_SSE_DISABLE", "1", true)
            .expect("HL_TRANSLIT_MIXED_SSE_DISABLE");
    }
    if force_body_owner_rotation {
        options
            .set("HL_TRANSLIT_BODY_OWNER_ROTATE_TEST", "1", true)
            .expect("HL_TRANSLIT_BODY_OWNER_ROTATE_TEST");
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

fn run_with_arguments(
    executable: &Path,
    translit: &str,
    extra: &[&[u8]],
    force_provenance_miss: bool,
    exhaust_body_owners: bool,
    force_fs_authority: bool,
    disable_mixed_sse: bool,
) -> (Vec<u8>, i32, Backend) {
    run_with_arguments_internal(
        executable,
        translit,
        extra,
        force_provenance_miss,
        exhaust_body_owners,
        force_fs_authority,
        disable_mixed_sse,
        false,
    )
}

fn run_with_body_owner_rotation(executable: &Path) -> (Vec<u8>, i32, Backend) {
    run_with_arguments_internal(executable, "1", &[], false, false, false, false, true)
}

fn run(executable: &Path, translit: &str) -> (Vec<u8>, i32, Backend) {
    run_with_arguments(executable, translit, &[], false, false, false, false)
}

fn run_with_jcc_controls(executable: &Path, disable_link: bool) -> (Vec<u8>, i32, Backend) {
    let captured = Arc::new(CapturedOutput::default());
    let mut options = Options::default();
    options.set("HL_TRANSLIT", "1", true).expect("HL_TRANSLIT");
    options.set("HL_C_DIAGNOSTICS", "1", true).expect("HL_C_DIAGNOSTICS");
    options
        .set("HL_TRANSLIT_JCC_IBTC_DISABLE", "1", true)
        .expect("HL_TRANSLIT_JCC_IBTC_DISABLE");
    options
        .set("HL_TRANSLIT_DIRECT_JMP_IBTC_DISABLE", "1", true)
        .expect("HL_TRANSLIT_DIRECT_JMP_IBTC_DISABLE");
    if disable_link {
        options
            .set("HL_TRANSLIT_JCC_LINK_DISABLE", "1", true)
            .expect("HL_TRANSLIT_JCC_LINK_DISABLE");
    }
    let plan = RuntimePlan {
        rootfs: None,
        executable_host: Some(executable.as_os_str().as_encoded_bytes().to_vec()),
        arguments: vec![executable.as_os_str().as_encoded_bytes().to_vec()],
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

fn run_with_jcc_ibtc_disabled(executable: &Path) -> (Vec<u8>, i32, Backend) {
    run_with_jcc_controls(executable, false)
}

fn run_with_jcc_link_disabled(executable: &Path) -> (Vec<u8>, i32, Backend) {
    run_with_jcc_controls(executable, true)
}

fn run_with_perf_map(executable: &Path, directory: &Path, force_two_rotations: bool) -> (Vec<u8>, i32, Backend) {
    let captured = Arc::new(CapturedOutput::default());
    let mut options = Options::default();
    options.set("HL_TRANSLIT", "1", true).unwrap();
    options.set("HL_C_DIAGNOSTICS", "1", true).unwrap();
    options
        .set("HL_TRANSLIT_PERF_MAP", directory.to_str().unwrap(), true)
        .unwrap();
    if executable.file_stem().and_then(|name| name.to_str()) == Some("perf_map_fork_exec") {
        options
            .set("HL_TRANSLIT_PERF_FRESH_ROLLOVER_TEST", "1", true)
            .unwrap();
    }
    if force_two_rotations {
        options.set("HL_TRANSLIT_BODY_OWNER_ROTATE_TEST", "1", true).unwrap();
    }
    let plan = RuntimePlan {
        rootfs: None,
        executable_host: Some(executable.as_os_str().as_encoded_bytes().to_vec()),
        arguments: vec![executable.as_os_str().as_encoded_bytes().to_vec()],
        environment: Vec::new(),
        result_path: None,
        options,
        box_policy: Default::default(),
    };
    let streams = StandardStreams::default().with_output(captured.clone());
    let engine = Engine::with_streams(GuestIsa::X86_64, plan, streams).unwrap();
    engine.start().unwrap();
    let exit = engine.wait().unwrap();
    engine.destroy().unwrap();
    let out = captured.out.lock().unwrap().clone();
    let report = backend(&captured.err.lock().unwrap());
    (out, exit.guest_status, report)
}

#[test]
fn transliterated_blocks_publish_perf_map_identities() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "forward_jump");
    let maps = work.path().join("maps");
    std::fs::create_dir(&maps).unwrap();
    let (output, status, backend) = run_with_perf_map(&executable, &maps, false);
    assert_eq!(status, 0);
    assert_eq!(output, b"42\n");
    let files: Vec<_> = std::fs::read_dir(&maps)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(files.len(), 2, "{files:?}");
    let map = files
        .iter()
        .find(|path| path.file_name().unwrap().to_string_lossy().starts_with("perf-"))
        .unwrap();
    let dump = files
        .iter()
        .find(|path| path.file_name().unwrap().to_string_lossy().starts_with("jit-"))
        .unwrap();
    let dump_bytes = std::fs::read(dump).unwrap();
    assert_eq!(&dump_bytes[..4], &0x4A695444u32.to_ne_bytes());
    let text = std::fs::read_to_string(map).unwrap();
    let mut records = 0;
    let mut jcc_helpers = 0;
    let mut direct_jmp_helpers = 0;
    for line in text.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        assert_eq!(fields.len(), 3, "{line}");
        assert!(u64::from_str_radix(fields[0], 16).unwrap() != 0, "{line}");
        assert!(u64::from_str_radix(fields[1], 16).unwrap() != 0, "{line}");
        assert!(fields[2].starts_with("hl_tl_"), "{line}");
        assert!(!fields[2].contains("unfingerprinted"), "{line}");
        if fields[2] == "hl_tl_helper_jcc_ibtc" {
            jcc_helpers += 1;
            continue;
        }
        if fields[2] == "hl_tl_helper_direct_jmp_ibtc" {
            direct_jmp_helpers += 1;
            continue;
        }
        assert!(fields[2].contains("_g"), "{line}");
        assert!(fields[2].contains("_gl"), "{line}");
        assert!(fields[2].contains("_i"), "{line}");
        records += 1;
    }
    assert!(
        jcc_helpers == 1 && direct_jmp_helpers == 1 && records > 0 && records as u64 == backend.blocks,
        "jcc_helpers={jcc_helpers} direct_jmp_helpers={direct_jmp_helpers} records={records} {}",
        backend.line
    );
}

#[test]
fn forked_translators_publish_process_owned_perf_files() {
    let work = TempDir::new().unwrap();
    let run = |name: &str| {
        let executable = fixture(work.path(), name);
        let maps = work.path().join(format!("maps-{name}"));
        std::fs::create_dir(&maps).unwrap();
        let (output, status, backend) = run_with_perf_map(&executable, &maps, true);
        assert_eq!(status, 0);
        let output = String::from_utf8(output).unwrap();
        assert!(output.starts_with("fork-map=42 warm=10752 child=0 "), "{output}");
        let field = |field_name: &str| {
            output
                .split_whitespace()
                .find_map(|field| field.strip_prefix(field_name))
                .unwrap_or_else(|| panic!("missing {field_name} in {output}"))
        };
        let parent_pid = field("parent-pid=");
        let child_pid = field("child-pid=");
        let caller = field("caller=").trim_start_matches("0x");
        let target = field("target=").trim_start_matches("0x");
        let names: Vec<_> = std::fs::read_dir(&maps)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names.iter().filter(|name| name.starts_with("perf-")).count(),
            4,
            "{names:?}"
        );
        assert_eq!(
            names.iter().filter(|name| name.starts_with("jit-")).count(),
            4,
            "{names:?}"
        );
        for pid in [parent_pid, child_pid] {
            let mut process_maps = std::fs::read_dir(&maps)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| {
                    path.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with(&format!("perf-{pid}-"))
                })
                .collect::<Vec<_>>();
            process_maps.sort();
            let expected = if pid == parent_pid { 3 } else { 1 };
            assert_eq!(process_maps.len(), expected, "pid={pid} maps={process_maps:?}");
            if expected == 3 {
                let names = process_maps
                    .iter()
                    .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
                    .collect::<Vec<_>>();
                let rx = names[0].split("-rx").nth(1).unwrap();
                assert!(names.iter().all(|name| name.ends_with(rx)), "in-place generations changed RX: {names:?}");
            }
            let mut all = String::new();
            for map in process_maps {
                let text = std::fs::read_to_string(&map).unwrap_or_else(|error| panic!("{}: {error}", map.display()));
                assert_eq!(
                    text.lines()
                        .filter(|line| line.ends_with(" hl_tl_helper_jcc_ibtc"))
                        .count(),
                    1,
                    "{}:\n{text}",
                    map.display()
                );
                assert_eq!(
                    text.lines()
                        .filter(|line| line.ends_with(" hl_tl_helper_direct_jmp_ibtc"))
                        .count(),
                    1,
                    "{}:\n{text}",
                    map.display()
                );
                assert!(
                    text.lines()
                        .any(|line| line.contains("_g") && line.contains("_gl") && line.contains("_i")),
                    "ordinary block absent from {}:\n{text}",
                    map.display()
                );
                all.push_str(&text);
            }
            assert!(all.contains(caller), "caller {caller} absent for pid {pid}:\n{all}");
            assert!(all.contains(target), "target {target} absent for pid {pid}:\n{all}");
        }
        assert_eq!(backend.body_owner_low_rotations, 2, "{}", backend.line);
        backend
    };
    let one = run("perf_map_fork_one");
    let two = run("perf_map_fork_two");
    assert!(one.direct_call_ibtc_misses >= 2, "{}", one.shape_line);
    assert_eq!(two.direct_call_ibtc_misses, one.direct_call_ibtc_misses);
    assert_eq!(two.direct_call_ibtc_fills, one.direct_call_ibtc_fills);
    assert!(
        two.direct_call_ibtc_hits >= one.direct_call_ibtc_hits + 1,
        "one={}\ntwo={}",
        one.shape_line,
        two.shape_line
    );
}

#[test]
fn fork_exec_rebinds_perf_output_to_each_executable_arena() {
    // A live post-exec peer makes the test hook enter the production fresh-arena STW path.
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "perf_map_fork_exec");
    let maps = work.path().join("maps-fork-exec");
    std::fs::create_dir(&maps).unwrap();
    let (output, status, _backend) = run_with_perf_map(&executable, &maps, false);
    assert_eq!(status, 0);
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("post-exec pid="), "{output}");
    assert!(output.contains("parent pid="), "{output}");
    let child = output
        .split_whitespace()
        .find_map(|field| field.strip_prefix("child="))
        .expect("child pid");
    let mut child_maps = std::fs::read_dir(&maps)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(&format!("perf-{child}-"))
        })
        .collect::<Vec<_>>();
    child_maps.sort();
    assert!(child_maps.len() >= 2, "child={child} maps={child_maps:?}\n{output}");
    let identities = child_maps
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(identities.len(), child_maps.len(), "{child_maps:?}");
    for map in &child_maps {
        let text = std::fs::read_to_string(map).unwrap();
        assert!(text.contains(" hl_tl_helper_jcc_ibtc"), "{}:\n{text}", map.display());
        assert!(
            text.lines().any(|line| line.contains("_g") && line.contains("_gl") && line.contains("_i")),
            "{}:\n{text}",
            map.display()
        );
    }
    let rx = child_maps
        .iter()
        .filter_map(|path| path.file_name().unwrap().to_string_lossy().split("-rx").nth(1).map(str::to_owned))
        .collect::<std::collections::BTreeSet<_>>();
    assert!(rx.len() >= 2, "exec retained only stale RX identity: {child_maps:?}");
}

#[test]
#[ignore = "requires HL_PROFILE_TRANSLIT_EXECUTABLE and a host perf recording"]
fn a_host_profiler_resolves_transliterated_block_identities() {
    let executable =
        PathBuf::from(std::env::var_os("HL_PROFILE_TRANSLIT_EXECUTABLE").expect("HL_PROFILE_TRANSLIT_EXECUTABLE"));
    let (output, status, backend) = run_with_perf_map(&executable, Path::new("/tmp"), false);
    assert_eq!(status, 0);
    assert!(output.is_empty());
    assert!(backend.entries > 0, "{}", backend.line);
}

#[test]
fn fatal_signal_is_reported_once_by_the_safe_lifecycle_parent() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "fatal_signal");
    let captured = Arc::new(CapturedOutput::default());
    let mut options = Options::default();
    options.set("HL_TRANSLIT", "1", true).unwrap();
    options.set("HL_C_DIAGNOSTICS", "1", true).unwrap();
    let plan = RuntimePlan {
        rootfs: None,
        executable_host: Some(executable.as_os_str().as_encoded_bytes().to_vec()),
        arguments: vec![executable.as_os_str().as_encoded_bytes().to_vec()],
        environment: Vec::new(),
        result_path: None,
        options,
        box_policy: Default::default(),
    };
    let streams = StandardStreams::default().with_output(captured.clone());
    let engine = Engine::with_streams(GuestIsa::X86_64, plan, streams).unwrap();
    engine.start().unwrap();
    let exit = engine.wait().unwrap();
    engine.destroy().unwrap();
    assert_eq!(exit.kind, hl_engine::engine::ExitKind::Signal);
    assert_eq!(exit.guest_status, 11);

    let stderr = captured.err.lock().unwrap().clone();
    let text = String::from_utf8(stderr).unwrap();
    let records = text
        .lines()
        .filter(|line| line.starts_with("[diag] backend-tree "))
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 1, "{text}");
    let value = |name: &str| {
        records[0]
            .split_whitespace()
            .find_map(|field| field.strip_prefix(name))
            .and_then(|field| field.parse::<u64>().ok())
            .unwrap_or_else(|| panic!("missing {name} in {}", records[0]))
    };
    assert_eq!(value("claimed="), 1, "{}", records[0]);
    assert_eq!(value("completed="), 0, "{}", records[0]);
    assert_eq!(value("abnormal="), 1, "{}", records[0]);
    assert_eq!(value("missing="), 0, "{}", records[0]);
    assert_eq!(value("duplicate_finalize="), 0, "{}", records[0]);
    assert_eq!(
        value("crossings="),
        value("translated_entries=") + value("interpreted_entries="),
        "{}",
        records[0]
    );
    let reasons = (0..16).map(|reason| value(&format!("reason{reason}="))).sum::<u64>() + value("reason_other=");
    assert_eq!(reasons, value("crossings="), "{}", records[0]);
}

fn wide_profile(executable: &Path, termination: &[u8]) -> (i32, Vec<u8>) {
    let captured = Arc::new(CapturedOutput::default());
    let mut options = Options::default();
    options.set("HL_TRANSLIT", "1", true).unwrap();
    options.set("HL_C_DIAGNOSTICS", "1", true).unwrap();
    options.set("HL_TRANSLIT_PROFILE_WIDE_TEST", "1", true).unwrap();
    let plan = RuntimePlan {
        rootfs: None,
        executable_host: Some(executable.as_os_str().as_encoded_bytes().to_vec()),
        arguments: vec![executable.as_os_str().as_encoded_bytes().to_vec(), termination.to_vec()],
        environment: Vec::new(),
        result_path: None,
        options,
        box_policy: Default::default(),
    };
    let streams = StandardStreams::default().with_output(captured.clone());
    let engine = Engine::with_streams(GuestIsa::X86_64, plan, streams).unwrap();
    engine.start().unwrap();
    let exit = engine.wait().unwrap();
    engine.destroy().unwrap();
    let stderr = captured.err.lock().unwrap().clone();
    (exit.guest_status, stderr)
}

#[test]
fn full_width_translit_profile_is_complete_for_exit_and_exit_group() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "profile_termination");
    for termination in [b"exit".as_slice(), b"group".as_slice()] {
        let (status, stderr) = wide_profile(&executable, termination);
        assert_eq!(status, 0);
        assert_eq!(stderr.last(), Some(&b'\n'), "{}", String::from_utf8_lossy(&stderr));
        let text = String::from_utf8(stderr).unwrap();
        let lines: Vec<_> = text
            .lines()
            .filter(|line| line.starts_with("[prof] translit:"))
            .collect();
        assert_eq!(lines.len(), 1, "{text}");
        let line = lines[0];
        assert!(line.contains("natural_lea_lowered=18446744073709551615"), "{line}");
        assert!(line.contains("jcc_fall_executed=18446744073709551615"), "{line}");
        assert!(!line.contains("[diag]"), "{line}");
        if termination == b"group" {
            assert!(
                text.lines().any(|record| record.starts_with("[diag] boundary ")),
                "{text}"
            );
        }
    }
}

/// The whole contract: the backend selection must not be observable in the guest's output.
fn agrees(name: &str) -> Backend {
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
    transliterated_backend
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
    let (selected, selected_status, selected_backend) =
        run_with_arguments(&executable, "1", &[], false, true, false, false);
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
    let tree = agrees("procs");
    assert!(tree.root_pid > 0, "{}", tree.tree_line);
    assert_eq!(tree.claimed, 17, "{}", tree.tree_line);
    assert_eq!(tree.completed, tree.claimed, "{}", tree.tree_line);
    assert_eq!(
        (tree.abnormal, tree.missing, tree.duplicate_finalize),
        (0, 0, 0),
        "{}",
        tree.tree_line
    );
    assert_eq!(
        tree.translated_entries + tree.interpreted_entries,
        tree.crossings,
        "{}",
        tree.tree_line
    );
    assert!(
        tree.translated_steps >= tree.translated_entries,
        "every aggregated translated child entry must carry its immutable descriptor step count: {}",
        tree.tree_line
    );
    assert!(
        tree.translated_steps + tree.interpreted_steps >= tree.crossings,
        "process-tree instruction residence must cover every aggregated crossing: {}",
        tree.tree_line
    );
    assert_eq!(tree.reason_total, tree.crossings, "{}", tree.tree_line);
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

#[test]
fn next_sse_family_matches_native_for_unaligned_high_rip_alias_flags_and_prefixes() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "sse_next_family");
    let (interpreted, interpreted_status, _) = run(&executable, "0");
    let (selected, selected_status, selected_backend) = run(&executable, "1");
    let native = std::process::Command::new(&executable)
        .output()
        .expect("native next SSE family fixture");
    assert_eq!(selected_status, interpreted_status);
    assert_eq!(selected, interpreted);
    assert_eq!(native.status.code(), Some(selected_status));
    assert_eq!(native.stdout, selected);
    assert!(selected_backend.sse2_next_family_runs > 0, "{}", selected_backend.line);
    assert!(
        selected_backend.sse2_store_instructions >= 3,
        "{}",
        selected_backend.line
    );
}

#[test]
fn next_sse_family_fixture_contains_the_exact_dynamic_chain_and_extended_forms() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "sse_next_family");
    let image = std::fs::read(executable).unwrap();
    let contains = |bytes: &[u8]| image.windows(bytes.len()).any(|window| window == bytes);
    assert!(contains(&[0xf3, 0x0f, 0x6f, 0x47, 0x01]));
    assert!(contains(&[0xf3, 0x0f, 0x6f, 0x4f, 0x11]));
    assert!(contains(&[0x66, 0x0f, 0xdf, 0xc1]));
    assert!(contains(&[0xf3, 0x45, 0x0f, 0x6f, 0x40, 0x03]));
    assert!(contains(&[0xf3, 0x44, 0x0f, 0x6f, 0x0d]));
    assert!(contains(&[0x66, 0x45, 0x0f, 0xdf, 0xc1]));
    assert!(contains(&[0xf3, 0x0f, 0x7f]));
}

#[test]
fn sse_stores_match_native_with_unaligned_high_registers_and_flags() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "sse_store");
    let (interpreted, interpreted_status, _) = run(&executable, "0");
    let (selected, selected_status, selected_backend) = run(&executable, "1");
    let native = std::process::Command::new(&executable)
        .output()
        .expect("native SSE store fixture");
    assert_eq!(selected_status, interpreted_status);
    assert_eq!(selected, interpreted);
    assert_eq!(native.status.code(), Some(selected_status));
    assert_eq!(native.stdout, selected);
    assert!(
        selected_backend.sse2_store_movups > 0
            && selected_backend.sse2_store_movaps > 0
            && selected_backend.sse2_store_movdqu > 0,
        "store counters ups={} aps={} dqu={} displaced_memory_declined={} line={}",
        selected_backend.sse2_store_movups,
        selected_backend.sse2_store_movaps,
        selected_backend.sse2_store_movdqu,
        selected_backend.sse2_memory_declined,
        selected_backend.line
    );
    assert!(selected_backend.sse2_store_family_runs > 0, "{}", selected_backend.line);
    let image = std::fs::read(executable).unwrap();
    for opcode in [&[0x44, 0x0f, 0x11][..], &[0x44, 0x0f, 0x29], &[0xf3, 0x44, 0x0f, 0x7f]] {
        assert!(image.windows(opcode.len()).any(|window| window == opcode));
    }
}

#[test]
fn aligned_store_fault_reports_the_guest_instruction_and_does_not_write() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "sse_store_fault");
    let (interpreted, interpreted_status, _) = run(&executable, "0");
    let (selected, selected_status, selected_backend) = run(&executable, "1");
    let native = std::process::Command::new(&executable)
        .output()
        .expect("native aligned-store fault fixture");
    assert_eq!((selected_status, &selected), (interpreted_status, &interpreted));
    assert_eq!(native.status.code(), Some(selected_status));
    assert_eq!(native.stdout, selected);
    assert_eq!(selected, b"faults=1 unchanged=1\n");
    assert!(selected_backend.sse2_store_movaps > 0, "{}", selected_backend.line);
}

#[test]
fn executable_store_authority_keeps_the_store_in_the_interpreter() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "sse_store_authority");
    let (interpreted, interpreted_status, _) = run(&executable, "0");
    let (selected, selected_status, selected_backend) = run(&executable, "1");
    let native = std::process::Command::new(&executable)
        .output()
        .expect("native store-authority fixture");
    assert_eq!((selected_status, &selected), (interpreted_status, &interpreted));
    assert_eq!(native.status.code(), Some(selected_status));
    assert_eq!(native.stdout, selected);
    assert_eq!(selected, b"authority=16\n");
    assert!(selected_backend.declined > 0, "{}", selected_backend.line);
    assert_eq!(selected_backend.sse2_store_instructions, 0, "{}", selected_backend.line);
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
    assert!(selected_backend.shape_stitch_jmp > 0, "{}", selected_backend.shape_line);
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
        selected_backend.shape_stitch_cond_fall > 0 && selected_backend.shape_cond_taken > 0,
        "{}",
        selected_backend.shape_line
    );
    assert!(
        selected_backend.shape_jcc_taken_eligible > 0,
        "{}",
        selected_backend.shape_line
    );
    assert!(
        selected_backend.jcc_fall_admitted <= selected_backend.jcc_fall_candidates,
        "{}",
        selected_backend.line
    );
}

/// An already-published target links immutably; an otherwise-identical cold target keeps the dispatcher
/// exit. The interval timer makes an async kick race the linked path, while the fourth argument proves
/// RCX survives spill/poll/reload. Backend-shape must account every successful link as a chained JCC.
#[test]
fn an_already_published_same_page_taken_jcc_links_without_losing_irq_or_rcx() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "jcc_link");
    let (interpreted, interpreted_status, _) = run(&executable, "0");
    let (selected, selected_status, selected_backend) = run_with_jcc_ibtc_disabled(&executable);
    let (disabled, disabled_status, disabled_backend) = run_with_jcc_link_disabled(&executable);
    assert_eq!(selected_status, interpreted_status);
    assert_eq!(selected, interpreted);
    assert_eq!(disabled_status, interpreted_status);
    assert_eq!(disabled, interpreted);
    assert!(String::from_utf8_lossy(&selected).contains("signals=1"));
    assert!(selected_backend.jcc_link_admitted > 0, "{}", selected_backend.line);
    assert!(selected_backend.jcc_link_taken > 0, "{}", selected_backend.line);
    assert!(selected_backend.jcc_link_irq_fallback > 0, "{}", selected_backend.line);
    assert!(
        selected_backend.jcc_link_irq_fallback < selected_backend.jcc_link_taken,
        "{}",
        selected_backend.line
    );
    assert!(selected_backend.jcc_link_dispatcher > 0, "{}", selected_backend.line);
    assert_eq!(disabled_backend.jcc_link_admitted, 0, "{}", disabled_backend.line);
    assert_eq!(disabled_backend.jcc_link_taken, 0, "{}", disabled_backend.line);
    // IBTC OFF suppresses publication, not its byte-identical source scaffold,
    // so this counter now names only unrelated ordinary-dispatcher sources.
    // Those must stay stable while the direct-link fields above carry the AB proof.
    assert_eq!(
        disabled_backend.jcc_link_dispatcher, selected_backend.jcc_link_dispatcher,
        "{}\n{}",
        disabled_backend.line, selected_backend.line
    );
    assert_eq!(
        selected_backend.shape_jcc_taken_chained, selected_backend.jcc_link_taken,
        "{}\n{}",
        selected_backend.line, selected_backend.shape_line
    );
    assert_eq!(
        selected_backend.would_jmp_target_unmapped, disabled_backend.would_jmp_target_unmapped,
        "same-family linked ingress must retain the executed target JMP's publication disposition:\n{}\n{}",
        selected_backend.would_link_line, disabled_backend.would_link_line
    );
    assert!(
        selected_backend.would_jmp_target_unmapped > 1000,
        "the warmed target JMP must execute after its cold-target publication: {}",
        selected_backend.would_link_line
    );
    assert_eq!(
        selected_backend.would_jmp_eligible, disabled_backend.would_jmp_eligible,
        "linked ingress must not credit the unexecuted source JMP classification:\n{}\n{}",
        selected_backend.would_link_line, disabled_backend.would_link_line
    );
    assert_eq!(
        selected_backend.would_call_target_unmapped, disabled_backend.would_call_target_unmapped,
        "differing-family linked ingress must count the executed target CALL:\n{}\n{}",
        selected_backend.would_link_line, disabled_backend.would_link_line
    );
    // Settled external test engines predating backend-shape v5 remain valid
    // semantic oracles.  A current engine must also prove the typed CALL path.
    if selected_backend.shape_line.contains("direct_call_ibtc_emitted=") {
        assert!(
            selected_backend.direct_call_ibtc_emitted > 0,
            "{}",
            selected_backend.shape_line
        );
        assert!(
            selected_backend.direct_call_ibtc_hits > 1000,
            "the warmed direct CALL must execute through the shared target cache: {}",
            selected_backend.shape_line
        );
        assert!(
            selected_backend.direct_call_ibtc_misses > 0 && selected_backend.direct_call_ibtc_fills > 0,
            "the cold direct CALL path must publish before it becomes hot: {}",
            selected_backend.shape_line
        );
        assert_eq!(
            selected_backend.direct_call_ibtc_misses,
            selected_backend.direct_call_ibtc_fills + selected_backend.direct_call_ibtc_invalid_refusals,
            "{}",
            selected_backend.shape_line
        );
        assert!(
            selected_backend.direct_call_ibtc_irq <= selected_backend.direct_call_ibtc_emitted,
            "{}",
            selected_backend.shape_line
        );
    }
    assert_eq!(
        selected_backend.would_fall_candidate,
        selected_backend.shape_fallthrough
    );
    assert_eq!(selected_backend.would_jmp_candidate, selected_backend.shape_direct_jump);
    assert_eq!(
        selected_backend.would_call_candidate,
        selected_backend.shape_direct_call
    );
    assert!(
        selected_backend.shape_jcc_taken_dispatcher > 0,
        "{}",
        selected_backend.shape_line
    );
}

#[test]
#[ignore = "same-box mechanism benchmark; run only under the exclusive measurement lock"]
fn benchmark_same_binary_jcc_link_disabled_enabled_enabled_disabled() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "jcc_link");
    let mut reference = None;
    for disabled in [true, false, false, true] {
        let started = std::time::Instant::now();
        let (out, status, backend) = if disabled {
            run_with_jcc_link_disabled(&executable)
        } else {
            run(&executable, "1")
        };
        let elapsed = started.elapsed();
        assert_eq!(status, 0);
        if let Some(expected) = &reference {
            assert_eq!(&out, expected);
        } else {
            reference = Some(out);
        }
        eprintln!(
            "jcc_link disabled={} elapsed_ns={} admitted={} taken={} irq_fallback={} dispatcher={}",
            disabled,
            elapsed.as_nanos(),
            backend.jcc_link_admitted,
            backend.jcc_link_taken,
            backend.jcc_link_irq_fallback,
            backend.jcc_link_dispatcher
        );
    }
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
    assert!(
        selected_backend.shape_stitch_cond_fall > 0 && selected_backend.shape_fault > 0,
        "{}",
        selected_backend.shape_line
    );
    assert_eq!(selected_backend.shape_other, 0, "{}", selected_backend.shape_line);
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
        run_with_arguments(&executable, "1", &[b"stack"], true, false, false, false);
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

    // The low return-address half commits before the high half crosses into a
    // protected page.  The architectural CALL itself has not committed: RIP
    // and RSP must still identify the source instruction and original stack.
    let (selected_split, selected_split_status, _) =
        run_with_arguments(&executable, "1", &[b"split"], true, false, false, false);
    let native_split = std::process::Command::new(&executable)
        .arg("split")
        .output()
        .expect("native split stack-fault fixture");
    assert_eq!(selected_split_status, 0, "{}", String::from_utf8_lossy(&selected_split));
    assert_eq!(native_split.status.code(), Some(0));
    assert!(selected_split.ends_with(b"faults=1 r11=1 rip=1 rsp=1 low=1\n"));
    // A native CALL is one architectural eight-byte push and therefore leaves
    // no partial word. The emitted lowering deliberately uses two no-clobber
    // stores, so its first half is observable before the second faults.
    assert!(native_split.stdout.ends_with(b"faults=1 r11=1 rip=1 rsp=1 low=0\n"));
}

#[test]
fn ret_stack_faults_preserve_architecture_before_commit() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path(), "ret_stack_fault");
    for arguments in [&[][..], &[b"2".as_slice()][..], &[b"3".as_slice(), b"u".as_slice()][..],
                      &[b"2".as_slice(), b"u".as_slice()][..]] {
        let (selected, status, _) = run_with_arguments(&executable, "1", arguments, true, false, false, false);
        assert_eq!(status, 0, "{}", String::from_utf8_lossy(&selected));
        assert!(selected.ends_with(b"faults=1 rip=1 rsp=1 regs=1 flags=1\n"),
                "{}", String::from_utf8_lossy(&selected));
    }
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
/// differential above, so a `translit: displaced` receipt is mandatory rather than inferred. A caller
/// may additionally supply an owned perf-map directory; it is handed to the launch option store rather
/// than read ambiently by the engine.
fn captured_cc1_profile(root: &Path, argv_path: &Path, selected: &str, perf_map: Option<&Path>) {
    assert!(selected == "0" || selected == "1");
    let mut arguments: Vec<Vec<u8>> = std::fs::read(argv_path)
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
    options.set("HL_TRANSLIT", selected, true).unwrap();
    options.set("HL_C_DIAGNOSTICS", "1", true).unwrap();
    if let Some(directory) = perf_map {
        options
            .set_bytes("HL_TRANSLIT_PERF_MAP", directory.as_os_str().as_encoded_bytes(), true)
            .expect("HL_TRANSLIT_PERF_MAP");
    }
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

#[test]
fn canonical_cc1_profile_hands_off_caller_owned_perf_map_directory() {
    let work = TempDir::new().unwrap();
    let root = work.path().join("root");
    let executable = root.join("usr/bin/cc1-profile");
    let argv = root.join("work/cc1.argv");
    let maps = work.path().join("maps");
    std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
    std::fs::create_dir_all(argv.parent().unwrap()).unwrap();
    std::fs::create_dir(&maps).unwrap();
    std::fs::copy(displaced_fixture(work.path(), "profile_termination"), &executable).unwrap();
    std::fs::write(&argv, b"/usr/bin/cc1-profile\n-o\n/work/output.s\n").unwrap();

    captured_cc1_profile(&root, &argv, "1", Some(&maps));

    let files: Vec<_> = std::fs::read_dir(&maps)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(files.len(), 4, "two RX generations must each publish a map and jitdump: {files:?}");
    for prefix in ["perf-", "jit-"] {
        let selected = files
            .iter()
            .filter(|path| path.file_name().unwrap().to_string_lossy().starts_with(prefix))
            .collect::<Vec<_>>();
        assert_eq!(selected.len(), 2, "missing generation-owned {prefix} files in {files:?}");
        for file in selected {
            assert!(std::fs::metadata(file).unwrap().len() > 0, "{} is empty", file.display());
            if prefix == "perf-" {
                let text = std::fs::read_to_string(file).unwrap();
                assert_eq!(text.lines().filter(|line| line.ends_with(" hl_tl_helper_jcc_ibtc")).count(), 1,
                           "{}:\n{text}", file.display());
                assert_eq!(text.lines().filter(|line| line.ends_with(" hl_tl_helper_direct_jmp_ibtc")).count(), 1,
                           "{}:\n{text}", file.display());
                assert!(text.lines().any(|line| line.contains("_g") && line.contains("_gl") && line.contains("_i")),
                        "ordinary block absent from {}:\n{text}", file.display());
            }
        }
    }
}

#[test]
#[ignore = "requires HL_PROFILE_CC1_ROOT and HL_PROFILE_CC1_ARGV"]
fn a_captured_cc1_runs_from_displaced_storage() {
    let root = PathBuf::from(std::env::var_os("HL_PROFILE_CC1_ROOT").expect("HL_PROFILE_CC1_ROOT"));
    let argv_path = PathBuf::from(std::env::var_os("HL_PROFILE_CC1_ARGV").expect("HL_PROFILE_CC1_ARGV"));
    let selected = std::env::var("HL_PROFILE_CC1_TRANSLIT").expect("HL_PROFILE_CC1_TRANSLIT");
    let perf_map = std::env::var_os("HL_PROFILE_CC1_PERF_MAP").map(PathBuf::from);
    captured_cc1_profile(&root, &argv_path, &selected, perf_map.as_deref());
}
