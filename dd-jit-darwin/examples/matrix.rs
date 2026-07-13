//! dd-jit-darwin matrix runner — runs the engine × case matrix and prints a grouped, timed report.
//! This is the optional developer runner; `cargo test -p dd-jit-darwin --test suite` is the CI authority
//! and shares the SAME case registry (tests/engine_matrix, included below via #[path]).
//!
//!   cargo run -p dd-jit-darwin --example matrix                 # everything, every engine
//!   cargo run -p dd-jit-darwin --example matrix -- container    # only groups/cases matching "container"
//!   cargo run -p dd-jit-darwin --example matrix -- -e x86_64    # only the x86-64 engine
//!   cargo run -p dd-jit-darwin --example matrix -- --list       # list groups + cases without running

use crate::support::{run, run_perf, Ctx, Engine, Status};
use std::time::Instant;

// The product-neutral engine-test harness, included crate-locally (was the `dd-tests` dev-dep).
#[path = "../tests/support/mod.rs"]
mod support;
// Share the ONE Rust case registry with tests/suite.rs (no second catalog).
#[path = "../tests/engine_matrix/mod.rs"]
mod cases;
// report lives in a non-example subdir so cargo does not treat it as its own example target.
#[path = "matrix_support/report.rs"]
mod report;

fn parse_engine(s: &str) -> Option<Engine> {
    match s {
        "linux/aarch64" | "aarch64" | "arm64" => Some(Engine::LinuxAarch64),
        "linux/x86_64" | "x86_64" | "amd64" => Some(Engine::LinuxX86_64),
        "darwin/aarch64" | "darwin" | "macos" => Some(Engine::DarwinAarch64),
        _ => None,
    }
}

/// Machine-readable status tag for a perf row (skips never reach here).
fn status_label(st: &Status) -> &'static str {
    match st {
        Status::Pass => "pass",
        Status::Fail(_) => "fail",
        Status::Xfail(_) => "xfail",
        Status::Xpass => "xpass",
        Status::Skip(_) => "skip",
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let (mut engine_filter, mut name_filter, mut list) = (None, None, false);
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-e" | "--engine" => engine_filter = it.next().and_then(|s| parse_engine(s)),
            "--list" => list = true,
            "-h" | "--help" => {
                eprintln!("usage: matrix [--engine aarch64|x86_64] [--list] [name-filter]");
                return;
            }
            other => name_filter = Some(other.to_string()),
        }
    }
    let engines: Vec<Engine> = Engine::ALL
        .into_iter()
        .filter(|e| engine_filter.map_or(true, |f| f == *e))
        .collect();
    let matches = |g: &str, c: &str| {
        name_filter
            .as_ref()
            .map_or(true, |n| g.contains(n.as_str()) || c.contains(n.as_str()))
    };

    // Which engines MUST be present for this run to be a valid gate. The DEFAULT full run must exercise
    // ALL three engines — if the dd-jit build failed to compile one (empty `DDJIT_<T>` env), its whole
    // lane would otherwise SILENTLY skip and the matrix would still report "0 failed" while the primary
    // arch was completely dark (the regression that shipped v0.9.56). An explicit `-e`/ENGINE=
    // narrowed run is the escape hatch: it opts into a single engine, so only that one is required.
    // A `FILTER=` narrows CASES, not engines, so it does NOT relax the all-three invariant.
    let required: Vec<Engine> = match engine_filter {
        Some(f) => vec![f],
        None => Engine::ALL.to_vec(),
    };
    let missing: Vec<Engine> = required
        .iter()
        .copied()
        .filter(|e| !e.available())
        .collect();
    // Env-var-style short name ("linux/aarch64" -> "linux_aarch64") for the footer / red gate lines.
    let ekey = |e: Engine| e.label().replace('/', "_");

    if list {
        let mut n = 0;
        for g in cases::all() {
            println!("{}", g.name);
            for c in &g.cases {
                n += 1;
                println!(
                    "  {:<16} [{}]",
                    c.name,
                    c.engines
                        .iter()
                        .map(|e| e.label())
                        .collect::<Vec<_>>()
                        .join(",")
                );
            }
        }
        println!("\n{n} cases in {} groups", cases::all().len());
        return;
    }

    eprintln!(
        "engines: {}",
        Engine::ALL
            .iter()
            .map(|e| format!(
                "{} {}",
                e.label(),
                if e.available() {
                    "✓"
                } else {
                    "✗(not built)"
                }
            ))
            .collect::<Vec<_>>()
            .join("   ")
    );

    let ctx = Ctx::discover();
    // PERF mode: `PERF=1 make test` (or `make perf`). Times each executed cell's guest execution (median
    // of PERF_N runs, compilation excluded) and — for Oracle cases — the native run too, then renders the
    // oracle-vs-JIT slowdown table + summary and dumps perf.csv/perf.json. OFF by default → the matrix
    // output below is byte-identical to before.
    let perf = std::env::var("PERF").is_ok();
    let perf_n: usize = std::env::var("PERF_N")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n >= 1)
        .unwrap_or(3);
    let mut perf_rows: Vec<report::Row> = Vec::new();
    let (mut pass, mut fail, mut skip, mut xfail, mut xpass, mut busy_ms) =
        (0u32, 0u32, 0u32, 0u32, 0u32, 0u128);
    let mut failures: Vec<String> = Vec::new();
    let mut xpasses: Vec<String> = Vec::new();
    let mut slowest: Vec<(u128, String)> = Vec::new();
    let wall = Instant::now();

    for g in cases::all() {
        let group_cases: Vec<_> = g.cases.iter().filter(|c| matches(g.name, c.name)).collect();
        if group_cases.is_empty() {
            continue;
        }
        println!("\n\x1b[1m{}\x1b[0m", g.name);
        for c in group_cases {
            print!("  {:<16}", c.name);
            for &e in &engines {
                // Default: single wall-clock run() (the historical "111ms"). Perf: median jit execution
                // (compilation excluded) + timed native oracle, recorded as a Row for the table.
                let (st, ms) = if perf {
                    let t = run_perf(&ctx, c, e, perf_n);
                    let ms = t.jit_ms.unwrap_or(0);
                    if !matches!(t.status, Status::Skip(_)) {
                        perf_rows.push(report::Row {
                            group: g.name.to_string(),
                            test: c.name.to_string(),
                            arch: e.label(),
                            oracle_ms: t.oracle_ms,
                            jit_ms: ms,
                            status: status_label(&t.status),
                        });
                    }
                    (t.status, ms)
                } else {
                    let t0 = Instant::now();
                    let st = run(&ctx, c, e);
                    (st, t0.elapsed().as_millis())
                };
                match st {
                    Status::Skip(_) => {
                        skip += 1;
                        print!("  \x1b[90m· {}\x1b[0m", e.label());
                    }
                    Status::Pass => {
                        pass += 1;
                        busy_ms += ms;
                        slowest.push((ms, format!("{}/{} [{}]", g.name, c.name, e.label())));
                        print!("  \x1b[32m✓\x1b[0m {} \x1b[90m{ms}ms\x1b[0m", e.label());
                    }
                    Status::Fail(m) => {
                        fail += 1;
                        busy_ms += ms;
                        print!("  \x1b[31m✗ {} {ms}ms\x1b[0m", e.label());
                        failures.push(format!("{}/{} [{}]: {}", g.name, c.name, e.label(), m));
                    }
                    Status::Xfail(_) => {
                        xfail += 1;
                        busy_ms += ms;
                        print!("  \x1b[33mx {}\x1b[0m", e.label());
                    }
                    Status::Xpass => {
                        xpass += 1;
                        busy_ms += ms;
                        print!("  \x1b[35m✓! {}\x1b[0m", e.label());
                        xpasses.push(format!("{}/{} [{}]", g.name, c.name, e.label()));
                    }
                }
            }
            println!();
        }
    }

    // HARD GATE: a required engine whose JIT binary didn't build is a FAILURE, not a silent skip.
    // Fold each missing engine into the fail count and name it loudly, so a dark lane can never coexist
    // with a green "0 failed". `available()` is false when dd-jit's build.rs set an empty `DDJIT_<T>` env
    // after a failed C-engine compile (or the binary is otherwise absent).
    for &e in &missing {
        fail += 1;
        failures.push(format!(
            "engine {} MISSING — its JIT binary was not built (failed dd-jit compile / empty DDJIT_{} env); \
             the ENTIRE {} lane was DARK (every case on it skipped, NOT tested)",
            e.label(), ekey(e).to_uppercase(), e.label()));
    }

    println!("\n{}", "─".repeat(56));
    for f in &failures {
        println!("\x1b[31m✗\x1b[0m {f}");
    }
    slowest.sort_unstable_by(|a, b| b.0.cmp(&a.0));
    if !slowest.is_empty() {
        let top: Vec<String> = slowest
            .iter()
            .take(3)
            .map(|(ms, n)| format!("{n} {ms}ms"))
            .collect();
        println!("\x1b[90mslowest: {}\x1b[0m", top.join(", "));
    }
    for x in &xpasses {
        println!(
            "\x1b[35m✓!\x1b[0m {x} — XPASS (known failure now passes; remove the .xfail marker)"
        );
    }
    let color = if fail > 0 { "31" } else { "32" };
    let xf = if xfail > 0 {
        format!("  \x1b[33m{xfail} xfail\x1b[0m")
    } else {
        String::new()
    };
    let xp = if xpass > 0 {
        format!("  \x1b[35m{xpass} xpass\x1b[0m")
    } else {
        String::new()
    };
    println!("\x1b[1;{color}m{pass} passed\x1b[0m  {fail} failed{xf}{xp}  \x1b[90m{skip} skipped   {busy_ms}ms run, {}ms wall\x1b[0m",
        wall.elapsed().as_millis());

    // Engines FOOTER — ALWAYS printed so every gate shows at a glance which lanes actually ran. Each of
    // the three engines is OK (built), MISSING (required but not built → the hard failure above), or n/a
    // (not required in this `-e`-narrowed run). Red when any required engine is MISSING.
    let footer: Vec<String> = Engine::ALL
        .iter()
        .map(|&e| {
            let tag = if e.available() {
                "OK"
            } else if required.contains(&e) {
                "MISSING"
            } else {
                "n/a"
            };
            format!("{}={}", ekey(e), tag)
        })
        .collect();
    let fcolor = if missing.is_empty() { "90" } else { "1;31" };
    println!("\x1b[{fcolor}mengines: {}\x1b[0m", footer.join(" "));

    if perf {
        print!("{}", report::table(&perf_rows));
        print!("{}", report::summary(&perf_rows));
        println!("\x1b[90m(perf: median of {perf_n} run(s) per cell; oracle timed only for Oracle-checked cases)\x1b[0m");
        match report::write_machine(&ctx.cache, &perf_rows) {
            Ok((csv, json)) => println!(
                "\x1b[90mwrote {} and {}\x1b[0m",
                csv.display(),
                json.display()
            ),
            Err(e) => {
                eprintln!("\x1b[33mperf: failed to write machine-readable output: {e}\x1b[0m")
            }
        }
    }

    // XPASS is a red gate: an .xfail-marked case that now passes must have its marker removed, else a
    // later re-break silently reads as a "known" failure. (xfail alone stays green.)
    std::process::exit(if fail > 0 || xpass > 0 { 1 } else { 0 });
}
