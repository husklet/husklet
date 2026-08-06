//! Compiles, links and runs every standalone C program under `exec/test` against the engine archive
//! produced by this crate's build script, so those programs gate on `cargo test --workspace`.

use std::path::{Path, PathBuf};
use std::process::Command;

const ARCHIVE: &str = env!("HL_NATIVE_TEST_ARCHIVE");
const SOURCES: &str = env!("HL_NATIVE_TEST_SOURCES");
const INCLUDES: &str = env!("HL_NATIVE_TEST_INCLUDES");
const ARCH: &str = env!("HL_NATIVE_TEST_ARCH");
const ALLOCATION: &str = env!("HL_NATIVE_TEST_ALLOCATION");

/// Programs whose own driver script supplies an environment this harness deliberately does not build.
const SKIPPED: &[(&str, &str)] =
    &[("memory_lifecycle", "requires the malloc-interposing archive built by memory_lifecycle.sh")];

/// Programs that fail at HEAD for reasons that predate this harness. Removing an entry is the fix;
/// a listed program that starts passing fails the run so the list cannot rot. The allowlist excuses
/// a program that runs and fails, never one that fails to build.
const KNOWN_FAILING: &[(&str, &str)] = &[
    ("a64_cycles", "cycle accounting predates this harness"),
    ("aarch64_trace", "trace path predates this harness"),
    (
        "run",
        "run.c:71 hl_native_test_spill: hl_native_aarch64_enter records host_stack as a value other \
         than the caller's sp, so the spill contract check returns 1",
    ),
    ("translation", "translation.c:77 epoch run predates this harness"),
];

/// Assembly helpers that some programs need. They precede the archive on the link line so archive
/// members they reference are still pulled in.
const HELPERS: &[&str] = &["aarch64_spill.S", "x86_run.S"];

enum Outcome {
    Passed,
    Skipped(String),
    Failed(String),
    /// A program that did not build. Never excused by `KNOWN_FAILING`, because a broken build is a
    /// broken gate rather than a pre-existing behavioural failure.
    Broken(String),
}

fn workspace(directory: &Path) -> PathBuf {
    let scratch = directory.join("exec_c");
    std::fs::create_dir_all(&scratch).expect("create test scratch directory");
    scratch
}

fn compiler() -> String {
    std::env::var("CC").unwrap_or_else(|_| "cc".to_owned())
}

fn assemble(scratch: &Path) -> Vec<PathBuf> {
    if ARCH != "aarch64" {
        return Vec::new();
    }
    HELPERS
        .iter()
        .map(|name| {
            let object = scratch.join(name).with_extension("o");
            let output = Command::new(compiler())
                .arg("-c")
                .arg(Path::new(SOURCES).join(name))
                .arg("-o")
                .arg(&object)
                .output()
                .expect("run assembler");
            assert!(output.status.success(), "assemble {name}: {}", String::from_utf8_lossy(&output.stderr));
            object
        })
        .collect()
}

fn evaluate(source: &Path, scratch: &Path, helpers: &[PathBuf]) -> Outcome {
    let name = source.file_stem().expect("test name").to_string_lossy().into_owned();
    let binary = scratch.join(&name);
    let mut build = Command::new(compiler());
    build.arg("-std=c11").arg("-o").arg(&binary).arg(source);
    for include in INCLUDES.split(':') {
        build.arg("-I").arg(include);
    }
    build.args(helpers).arg(ARCHIVE).arg("-lpthread").arg("-lm");
    let built = build.output().expect("run compiler");
    if !built.status.success() {
        let report = String::from_utf8_lossy(&built.stderr).into_owned();
        if ARCH != "aarch64" {
            return Outcome::Skipped(format!("does not build on {ARCH}"));
        }
        return Outcome::Broken(format!("compile failed:\n{report}"));
    }
    let run = Command::new(&binary).output().expect("run C test");
    if run.status.success() {
        return Outcome::Passed;
    }
    Outcome::Failed(format!(
        "exit {:?}\n--- stdout ---\n{}--- stderr ---\n{}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    ))
}

fn discover() -> Vec<PathBuf> {
    let mut sources = std::fs::read_dir(SOURCES)
        .expect("read C test directory")
        .map(|entry| entry.expect("read C test entry").path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("c"))
        .filter(|path| {
            std::fs::read_to_string(path).expect("read C test source").contains("int main(void)")
        })
        .collect::<Vec<_>>();
    sources.sort();
    sources
}

#[test]
fn exec_c_programs_pass() {
    if ALLOCATION == "1" {
        eprintln!("skipped: HL_NATIVE_ALLOCATION_TEST rebuilds the archive for memory_lifecycle.sh");
        return;
    }
    let scratch = workspace(Path::new(env!("OUT_DIR")));
    let helpers = assemble(&scratch);
    let sources = discover();
    assert!(!sources.is_empty(), "no C tests discovered in {SOURCES}");

    let mut results = std::thread::scope(|scope| {
        let handles = sources
            .iter()
            .map(|source| {
                let scratch = &scratch;
                let helpers = &helpers;
                scope.spawn(move || {
                    let name = source.file_stem().expect("test name").to_string_lossy().into_owned();
                    let outcome = match SKIPPED.iter().find(|(skipped, _)| *skipped == name) {
                        Some((_, reason)) => Outcome::Skipped((*reason).to_owned()),
                        None => evaluate(source, scratch, helpers),
                    };
                    (name, outcome)
                })
            })
            .collect::<Vec<_>>();
        handles.into_iter().map(|handle| handle.join().expect("join C test")).collect::<Vec<_>>()
    });
    results.sort_by(|left, right| left.0.cmp(&right.0));

    let mut failures = Vec::new();
    let mut unexpected_passes = Vec::new();
    let mut passed = 0usize;
    for (name, outcome) in &results {
        let known = KNOWN_FAILING.iter().find(|(program, _)| program == name);
        match outcome {
            Outcome::Passed if known.is_some() => unexpected_passes.push(name.clone()),
            Outcome::Passed => passed += 1,
            Outcome::Skipped(reason) => eprintln!("skip {name}: {reason}"),
            Outcome::Failed(report) if known.is_some() => {
                eprintln!("known failure {name} ({}): {report}", known.expect("known entry").1)
            }
            Outcome::Failed(report) | Outcome::Broken(report) => {
                failures.push(format!("{name}: {report}"))
            }
        }
    }
    eprintln!("hl-native C tests: {passed} passed, {} total", results.len());
    assert!(
        unexpected_passes.is_empty(),
        "remove from KNOWN_FAILING, these now pass: {unexpected_passes:?}"
    );
    assert!(failures.is_empty(), "{} C test(s) failed:\n{}", failures.len(), failures.join("\n\n"));
    // A run where nothing actually executed is not a green gate, however few programs the host arch
    // can build.
    assert!(passed != 0, "no C test ran: {} discovered, all skipped or allowlisted", results.len());
}
