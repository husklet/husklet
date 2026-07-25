use super::corpus;
use super::*;

#[test]
fn angle_glsl_es_corpus_reaches_valid_modules() {
    let mut guard = match exec() {
        Some(g) => g,
        None => {
            eprintln!("glsl_es_corpus: no wgpu adapter -- skipping (headless without lavapipe)");
            return;
        }
    };
    let exec = &mut *guard;

    let mut passed = 0usize;
    let mut limits: Vec<(&str, &str, String)> = Vec::new();
    let mut unexpected_fail: Vec<(&str, String)> = Vec::new();
    let mut unexpected_pass: Vec<&str> = Vec::new();

    for c in corpus::GROUPS.iter().flat_map(|group| group.iter()) {
        match (c.expect, compile(exec, c.stage, c.entry, c.src)) {
            (Pass, Ok(())) => passed += 1,
            (Pass, Err(e)) => unexpected_fail.push((c.name, e)),
            (NagaLimit(reason), Err(e)) => limits.push((c.name, reason, e)),
            (NagaLimit(_), Ok(())) => unexpected_pass.push(c.name),
        }
    }

    let n: usize = corpus::GROUPS.iter().map(|group| group.len()).sum();
    eprintln!("\n=== ANGLE GLSL-ES corpus: {passed}/{n} reached a valid wgpu shader module ===");
    if !limits.is_empty() {
        eprintln!(
            "\n--- documented naga-24 limits ({}) -- skipped (still-fail, on the record) ---",
            limits.len()
        );
        for (name, reason, err) in &limits {
            eprintln!("  [naga-limit] {name}: {reason}\n               error: {err}");
        }
    }
    if !unexpected_pass.is_empty() {
        eprintln!("\n--- entries marked NagaLimit that now PASS (reclassify to Pass) ---");
        for name in &unexpected_pass {
            eprintln!("  {name}");
        }
    }
    if !unexpected_fail.is_empty() {
        eprintln!("\n--- UNEXPECTED failures (normalization gaps to fix) ---");
        for (name, err) in &unexpected_fail {
            eprintln!("  {name}: {err}");
        }
    }

    assert!(
        unexpected_fail.is_empty(),
        "{} corpus shader(s) failed to reach a valid module (see log)",
        unexpected_fail.len()
    );
    assert!(
        unexpected_pass.is_empty(),
        "{} shader(s) marked NagaLimit unexpectedly PASSED -- reclassify to Pass",
        unexpected_pass.len()
    );
}
