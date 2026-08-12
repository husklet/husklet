use super::{Baseline, ENGINE_BUILD, Isa, Provenance, Statistics, artifact, collect, pinning, revision, slot, wiring};

fn provenance() -> Provenance {
    Provenance {
        build_id: "abc".into(),
        revision: "0123456789ab".into(),
        rust_sha256: "feed".into(),
        host_load: "0.10/8".into(),
    }
}

#[test]
fn the_engine_binary_is_built_from_its_own_package_not_the_library() {
    let package = ENGINE_BUILD.iter().position(|argument| *argument == "-p").unwrap() + 1;
    assert_eq!(ENGINE_BUILD[package], "engine");
    assert_eq!(ENGINE_BUILD[ENGINE_BUILD.len() - 2..], ["--bin", "hl-engine"]);
}

#[test]
fn the_artifact_stream_yields_only_the_engine_executable() {
    let stream = concat!(
        "{\"reason\":\"compiler-artifact\",\"executable\":null}\n",
        "{\"reason\":\"compiler-artifact\",\"executable\":\"/t/release/hl-aarch64\"}\n",
        "{\"reason\":\"compiler-artifact\",\"executable\":\"/t/release/hl-engine\"}\n",
    );
    assert_eq!(artifact(stream), Some(std::path::PathBuf::from("/t/release/hl-engine")));
    assert_eq!(artifact("{\"reason\":\"build-finished\"}\n"), None);
}

#[test]
fn statistics_report_median_and_spread() {
    let statistics = Statistics::of(&[100, 104, 102]).unwrap();
    assert_eq!(
        (statistics.median, statistics.minimum, statistics.maximum),
        (102, 100, 104)
    );
    assert!((statistics.spread() - 4.0 / 102.0).abs() < 1e-9);
    assert!(Statistics::of(&[]).is_none());
    let outlier = Statistics::of(&[100, 101, 102, 103, 104, 105, 900]).unwrap();
    assert_eq!(outlier.median, 103);
    assert!(outlier.spread() < 0.05);
}

#[test]
fn pinning_selects_one_cpu_and_rejects_foreign_requests() {
    assert_eq!(slot("0-17", None, 4).unwrap(), (5, false));
    assert_eq!(slot("5", None, 12345).unwrap(), (5, true));
    assert_eq!(slot("0-3,8", Some(8), 0).unwrap(), (8, false));
    assert!(slot("0-3", Some(9), 0).is_err());
    assert!(slot("unknown", None, 0).is_err());
    assert!(pinning("0-17", None).unwrap().0 <= 17);
}

#[test]
fn defaulted_pins_spread_across_the_allowed_set() {
    let pins = (1000..1017)
        .map(|seed| slot("0-17", None, seed).unwrap().0)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(pins.len(), 17);
    assert!(!pins.contains(&0));
    assert_eq!(slot("0", None, 3).unwrap(), (0, true));
    assert_eq!(slot("0-3,8", None, 4).unwrap().0, 1);
    // An explicit request still wins whatever the seed is.
    assert_eq!(slot("0-17", Some(12), 999).unwrap().0, 12);
}

#[test]
fn each_guest_arch_names_the_lowering_it_covers() {
    assert!(Isa::X86.lowering().ends_with("x86_64"));
    assert!(Isa::Aarch64.lowering().ends_with("aarch64"));
    assert_ne!(Isa::X86.lowering(), Isa::Aarch64.lowering());
}

#[test]
fn absent_prerequisites_name_the_command_that_builds_them() {
    let absent = std::path::Path::new("/absent");
    let missing = super::missing(absent, absent, absent, absent, "amd64");
    assert_eq!(missing.len(), 4);
    assert!(missing[0].contains("make bench-guest BENCH_ARCH=amd64"));
    assert!(missing[1].contains("cargo build --release --locked -p engine --bin hl-engine"));
}

#[test]
fn wiring_never_selects_the_exec_wrapper_as_the_engine() {
    let (engine, runner) = wiring(std::path::Path::new("/build"), Isa::Aarch64);
    assert!(engine.ends_with("linux-production/hl-engine-linux-aarch64"));
    assert!(runner.ends_with("bin/hl-engine-runner"));
    assert_ne!(engine, runner);
}

#[test]
fn baseline_round_trips_engine_pin_and_samples() {
    let mut samples = std::collections::BTreeMap::new();
    samples.insert("compute".to_owned(), Statistics::of(&[41, 42, 43]).unwrap());
    let key = |arch: &str| (arch.to_owned(), "compute".to_owned(), "compute".to_owned());
    let text = Baseline::default().render(&provenance(), "arm64", "compute", &samples);
    let parsed = Baseline::parse(&text).unwrap();
    assert_eq!(parsed.engines["arm64"], "abc");
    assert_eq!(parsed.revision.as_deref(), Some("0123456789ab"));
    assert_eq!(parsed.samples[&key("arm64")], 42);
    assert!(Baseline::parse("sample\tonly\ttwo\n").is_err());

    let kept = Baseline::parse(&parsed.render(&provenance(), "amd64", "compute", &samples)).unwrap();
    assert_eq!(kept.samples.len(), 2);
    assert_eq!(kept.engines.len(), 2);
    assert!(kept.samples.contains_key(&key("arm64")));
}

#[test]
fn dirty_trees_are_never_recorded_and_revisions_are_reported() {
    assert!(!provenance().dirty());
    assert!(
        Provenance {
            revision: "0123456789ab-dirty".into(),
            ..provenance()
        }
        .dirty()
    );
    assert!(!revision().is_empty());
}

const HEADER: &str = "env,arch,phase,us,ok,guest_sha256,engine_sha256";

fn row(path: &std::path::Path, us: u64, engine: &str) {
    std::fs::write(path, format!("{HEADER}\nrust-engine,arm64,compute,{us},7,g,{engine}\n")).unwrap();
}

/// Writes one cycle file per provider, each carrying `phases` as `(name, ok)`.
fn cycle(directory: &std::path::Path, name: &str, arms: &[(&str, &[(&str, u64)])]) -> Vec<std::path::PathBuf> {
    arms.iter()
        .map(|(provider, phases)| {
            let path = directory.join(format!("{name}-{provider}.csv"));
            let mut text = format!("{HEADER}\n");
            for (phase, ok) in *phases {
                text.push_str(&format!("{provider},arm64,{phase},10,{ok},g,e\n"));
            }
            std::fs::write(&path, text).unwrap();
            path
        })
        .collect()
}

#[test]
fn collect_groups_samples_and_refuses_mixed_builds() {
    let directory = tempfile::tempdir().unwrap();
    let mut paths = Vec::new();
    for (index, us) in [10_u64, 12].into_iter().enumerate() {
        let path = directory.path().join(format!("cycle-{index}.csv"));
        row(&path, us, "same");
        paths.push(path);
    }
    let series = collect(&paths).unwrap();
    assert_eq!(series[&("compute".to_owned(), "rust-engine".to_owned())], [10, 12]);

    let foreign = directory.path().join("foreign.csv");
    row(&foreign, 11, "other");
    paths.push(foreign);
    assert!(collect(&paths).unwrap_err().contains("different trees"));
}

#[test]
fn a_healthy_run_where_every_arm_did_the_same_work_still_yields_a_series() {
    let directory = tempfile::tempdir().unwrap();
    let full: &[(&str, u64)] = &[("compute", 40), ("file", 400), ("syscall", 11)];
    let mut paths = cycle(directory.path(), "c1", &[("c-engine", full), ("rust-engine", full)]);
    paths.extend(cycle(
        directory.path(),
        "c2",
        &[("c-engine", full), ("rust-engine", full)],
    ));
    let series = collect(&paths).unwrap();
    assert_eq!(series[&("compute".to_owned(), "rust-engine".to_owned())].len(), 2);
}

/// The syscall phase used to be excluded from comparison because its checksum was a
/// sum of thread ids. It now reports a tid-derived invariant, so a divergence there is
/// a real disagreement and must be refused like any other phase.
#[test]
fn a_divergent_syscall_checksum_is_no_longer_waved_through() {
    let directory = tempfile::tempdir().unwrap();
    let paths = cycle(
        directory.path(),
        "c1",
        &[
            ("c-engine", &[("compute", 40), ("syscall", 21)]),
            ("rust-engine", &[("compute", 40), ("syscall", 99)]),
        ],
    );
    let error = collect(&paths).unwrap_err();
    assert!(error.contains("phase syscall did unequal work"), "{error}");
}

/// The checksum-parity gate is structurally blind to a timebase divergence: the work
/// counts are equal by construction and only `us=` moves, which is the number the
/// verdict is made of. The timebase row is the arm that is not blind.
#[test]
fn an_arm_whose_clock_forged_its_own_speedup_is_refused_by_the_timebase_row() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("forged.csv");
    std::fs::write(
        &path,
        format!("{HEADER}\nrust-engine,arm64,timebase,2615,21,g,e\nrust-engine,arm64,compute,2,40,g,e\n"),
    )
    .unwrap();
    let error = collect(std::slice::from_ref(&path)).unwrap_err();
    assert!(
        error.contains("rust-engine") && error.contains("divergent guest timebase"),
        "{error}"
    );

    std::fs::write(
        &path,
        format!("{HEADER}\nrust-engine,arm64,timebase,101848,1,g,e\nrust-engine,arm64,compute,2,40,g,e\n"),
    )
    .unwrap();
    assert!(collect(&[path]).is_ok());
}

#[test]
fn an_arm_that_truncated_a_phase_is_refused_by_name_with_both_checksums() {
    let directory = tempfile::tempdir().unwrap();
    let paths = cycle(
        directory.path(),
        "c1",
        &[
            ("c-engine", &[("compute", 40), ("file", 400)]),
            ("rust-engine", &[("compute", 40), ("file", 137)]),
        ],
    );
    let error = collect(&paths).unwrap_err();
    assert!(error.contains("phase file did unequal work"), "{error}");
    assert!(
        error.contains("c-engine ok=400") && error.contains("rust-engine ok=137"),
        "{error}"
    );
    assert!(!error.contains("phase compute"), "{error}");
}

#[test]
fn an_arm_that_never_reached_a_phase_is_refused_as_missing_not_as_disagreement() {
    let directory = tempfile::tempdir().unwrap();
    let paths = cycle(
        directory.path(),
        "c1",
        &[
            ("c-engine", &[("compute", 40), ("file", 400)]),
            ("rust-engine", &[("compute", 40)]),
        ],
    );
    let error = collect(&paths).unwrap_err();
    assert!(error.contains("phase file has no rows for rust-engine"), "{error}");
}

#[test]
fn an_arm_with_fewer_cycles_is_refused_even_when_its_checksum_matches() {
    let directory = tempfile::tempdir().unwrap();
    let phases: &[(&str, u64)] = &[("compute", 40), ("syscall", 11)];
    let mut paths = cycle(directory.path(), "c1", &[("c-engine", phases), ("rust-engine", phases)]);
    paths.extend(cycle(directory.path(), "c2", &[("c-engine", phases)]));
    let error = collect(&paths).unwrap_err();
    assert!(error.contains("unequal sample counts"), "{error}");
    assert!(
        error.contains("c-engine n=2") && error.contains("rust-engine n=1"),
        "{error}"
    );
}

#[test]
fn a_run_that_skipped_the_native_arm_is_not_treated_as_a_missing_arm() {
    let directory = tempfile::tempdir().unwrap();
    let phases: &[(&str, u64)] = &[("compute", 40)];
    let paths = cycle(directory.path(), "c1", &[("c-engine", phases), ("rust-engine", phases)]);
    assert!(collect(&paths).is_ok());
}
