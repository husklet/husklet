//! Guard tests for the project's *gates* themselves — the tools that are supposed to go RED when the
//! engine/test surface regresses. A gate that silently exits green while measuring nothing is worse than
//! no gate at all (it hides the very regressions it exists to catch). These tests assert the gates FAIL
//! loudly on the bad conditions.

use dd_tests::{gate_failures, Cell, Engine, Status};
use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn coverage_sh() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tools/coverage.sh")
}

/// Run coverage.sh with extra env, returning its exit code (or -1 if it didn't exit normally).
fn run_coverage(mode: &str, env: &[(&str, &str)]) -> i32 {
    let mut cmd = Command::new("bash");
    cmd.arg(coverage_sh()).arg(mode).current_dir(repo_root());
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output()
        .expect("spawn coverage.sh")
        .status
        .code()
        .unwrap_or(-1)
}

// ---------------------------------------------------------------------------------------------------
// Coverage tool — "Coverage Tool Uses Stale Engine Paths and Exits Green" (P0 false-green).
// The static lens must scan the real runtime tree; a missing/moved tree or an empty scan must be FATAL,
// never a green "handled 0 / N ... exit 0". (RT is overridden via DDCOV_RT.)
// ---------------------------------------------------------------------------------------------------

#[test]
fn coverage_static_scans_existing_runtime_sources() {
    // Against the real in-repo runtime sources the static scan must succeed (proves it actually found
    // and parsed the handler modules — the guard does not trip on a healthy tree).
    assert_eq!(
        run_coverage("static", &[]),
        0,
        "coverage.sh static must exit 0 against the real runtime tree"
    );
}

#[test]
fn coverage_static_is_fatal_when_runtime_tree_is_missing() {
    let code = run_coverage("static", &[("DDCOV_RT", "/nonexistent/dd-coverage-guard")]);
    assert_ne!(code, 0, "a missing runtime tree must be fatal, got exit {code}");
}

#[test]
fn coverage_report_does_not_greenlight_a_broken_scan() {
    // `report` writes the authoritative SYSCALL-COVERAGE.md; it must refuse to run against a tree whose
    // handler modules exist but parse to zero handled syscalls, rather than write a "0 handled" doc.
    let root = std::env::temp_dir().join("dd-cov-empty-guard");
    let sc = root.join("os/linux/syscall");
    let tr = root.join("translate/x86_64");
    std::fs::create_dir_all(&sc).unwrap();
    std::fs::create_dir_all(&tr).unwrap();
    for m in [
        "sysv", "mem", "signal", "time", "io", "aio", "fs", "proc", "net", "event", "misc", "rare",
    ] {
        std::fs::write(sc.join(format!("{m}.c")), b"// no nr-switch here\n").unwrap();
    }
    std::fs::write(tr.join("sysmap.h"), b"// no map\n").unwrap();
    let code = run_coverage("report", &[("DDCOV_RT", root.to_str().unwrap())]);
    assert_ne!(code, 0, "report against a zero-handler tree must be fatal, got exit {code}");
}

#[test]
fn coverage_dynamic_fails_when_required_engines_are_missing() {
    // A dynamic run that can't find the JIT engines has measured nothing; it must be fatal, not a green
    // "no hits" report. The override forces the not-built branch deterministically.
    let code = run_coverage(
        "dynamic",
        &[
            ("DDCOV_ENGINE_A", "/nonexistent/ddjit-linux_aarch64"),
            ("DDCOV_ENGINE_X", "/nonexistent/ddjit-linux_x86_64"),
        ],
    );
    assert_ne!(code, 0, "dynamic coverage with no engines must be fatal, got exit {code}");
}

// ---------------------------------------------------------------------------------------------------
// test-ci matrix gate — "`test-ci` Can Pass a Dark or Stale Matrix" (P0 false-green).
// `make test-ci` (= cargo test -p dd-tests) shares `gate_failures`, which must reject XPASS, dark
// (unbuilt) engine lanes, and a matrix that passed nothing.
// ---------------------------------------------------------------------------------------------------

fn pass_cell(e: Engine) -> Cell {
    Cell { name: "g/c".into(), engine: e, status: Status::Pass }
}
// Every engine "available" — isolates the guard under test from build state.
fn all_available(_: Engine) -> bool {
    true
}

#[test]
fn test_ci_matrix_passes_on_a_healthy_full_run() {
    let cells: Vec<Cell> = Engine::ALL.iter().map(|&e| pass_cell(e)).collect();
    let f = gate_failures(&Engine::ALL, &all_available, &cells);
    assert!(f.is_empty(), "healthy matrix must be green, got: {f:?}");
}

#[test]
fn test_ci_matrix_fails_xpass() {
    let mut cells: Vec<Cell> = Engine::ALL.iter().map(|&e| pass_cell(e)).collect();
    cells.push(Cell { name: "g/known".into(), engine: Engine::LinuxAarch64, status: Status::Xpass });
    let f = gate_failures(&Engine::ALL, &all_available, &cells);
    assert!(
        f.iter().any(|m| m.contains("XPASS")),
        "an XPASS cell must fail the gate, got: {f:?}"
    );
}

#[test]
fn test_ci_matrix_fails_dark_engine_lanes() {
    // Every engine passed, BUT one required engine is not built (its lane was actually dark/skipped).
    let cells: Vec<Cell> = Engine::ALL.iter().map(|&e| pass_cell(e)).collect();
    let avail = |e: Engine| e != Engine::LinuxX86_64; // x86_64 binary missing
    let f = gate_failures(&Engine::ALL, &avail, &cells);
    assert!(
        f.iter().any(|m| m.contains("MISSING") && m.contains("x86_64")),
        "a required-but-unbuilt engine lane must fail the gate, got: {f:?}"
    );
}

#[test]
fn test_ci_matrix_fails_empty_or_all_skipped() {
    // A matrix where every cell skipped (no Pass) must not report green.
    let cells: Vec<Cell> = Engine::ALL
        .iter()
        .map(|&e| Cell { name: "g/c".into(), engine: e, status: Status::Skip("no bin".into()) })
        .collect();
    let f = gate_failures(&Engine::ALL, &all_available, &cells);
    assert!(
        f.iter().any(|m| m.contains("no case passed")),
        "an all-skipped/empty matrix must fail the gate, got: {f:?}"
    );
    // Truly empty selection likewise.
    let f2 = gate_failures(&Engine::ALL, &all_available, &[]);
    assert!(!f2.is_empty(), "an empty matrix must fail the gate");
}

#[test]
fn test_ci_matrix_xfail_and_skip_stay_green() {
    // xfail (known failure) and skip must NOT fail the gate, as long as the lanes are covered by passes.
    let mut cells: Vec<Cell> = Engine::ALL.iter().map(|&e| pass_cell(e)).collect();
    cells.push(Cell { name: "g/x".into(), engine: Engine::LinuxAarch64, status: Status::Xfail("known".into()) });
    cells.push(Cell { name: "g/s".into(), engine: Engine::LinuxAarch64, status: Status::Skip("n/a".into()) });
    let f = gate_failures(&Engine::ALL, &all_available, &cells);
    assert!(f.is_empty(), "xfail/skip alongside passes must stay green, got: {f:?}");
}

// ---------------------------------------------------------------------------------------------------
// GUI matrix coverage — every gui_matrix/*.c probe must have a home: either built by the Makefile
// (in PROBES) or explicitly listed in the documented exclusion table. A probe source that sits in the
// tree but in neither is a SILENT coverage gap (a rendering regression it would catch goes ungated).
// ---------------------------------------------------------------------------------------------------

/// Probes intentionally NOT built by the matrix, each with an owner/reason. Keep empty unless a probe
/// genuinely cannot be gated yet; the point is that every exclusion is a deliberate, documented choice.
const GUI_MATRIX_EXCLUSIONS: &[(&str, &str)] = &[
    // ("probe_name", "owner: reason"),
];

#[test]
fn test_every_gui_matrix_probe_is_gated_or_documented() {
    let dir = repo_root().join("dd-tests/guests/gui_matrix");
    let makefile = std::fs::read_to_string(dir.join("Makefile")).expect("read gui_matrix Makefile");
    // Collect every token that appears in the Makefile (probe names appear in the *_PROBES lists and
    // their build rules), so a probe listed anywhere in the matrix counts as gated.
    let gated: std::collections::HashSet<&str> = makefile.split_whitespace().collect();
    let excluded: std::collections::HashSet<&str> =
        GUI_MATRIX_EXCLUSIONS.iter().map(|(n, _)| *n).collect();

    let mut orphans = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("read gui_matrix dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("c") {
            continue;
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap().to_string();
        if !gated.contains(stem.as_str()) && !excluded.contains(stem.as_str()) {
            orphans.push(stem);
        }
    }
    orphans.sort();
    assert!(
        orphans.is_empty(),
        "gui_matrix probe sources are neither built by the Makefile nor in the documented exclusion \
         table (silent coverage gap) — add them to the matrix or GUI_MATRIX_EXCLUSIONS: {orphans:?}"
    );
}
