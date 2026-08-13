use super::{BTreeMap, Row, Step};
use crate::benchmark::evidence::HostLoad;

fn row(key: &str, arm: &str, us: u64) -> Row {
    Row {
        key: key.into(),
        workload: "malloc".into(),
        layout: "plain".into(),
        cell: "EE".into(),
        round: 0,
        position: 0,
        arm: arm.into(),
        output: "same".into(),
        output_frame: "frame".into(),
        diagnostic: None,
        phases: [("malloc".into(), super::super::evidence::Phase { us, ok: "same".into() })].into(),
        host_load: vec![HostLoad {
            before: 0.1,
            after: 0.2,
        }],
    }
}

#[test]
fn null_qualification_is_fail_closed() {
    assert!((super::qualify_null(&[1.004, 0.997, 1.003, 0.998], false).unwrap() - 0.004).abs() < 1e-9);
    // AB,BA,BA,AB has balanced order and temporal strata here. Grouping by
    // round parity would incorrectly compare [1.02, 1.02] with [0.98, 0.98].
    assert!((super::qualify_null(&[1.02, 0.98, 1.02, 0.98], false).unwrap() - 0.02).abs() < 1e-9);
    assert!(super::qualify_null(&[1.02; 4], false).is_err());
    assert!(super::qualify_null(&[1.051, 0.983, 0.983, 0.983], false).is_err());
}

#[test]
fn control_correction_accounts_for_drift_in_both_arms() {
    let measured = 1.07;
    let base_drift = 0.02;
    let candidate_drift = 0.02;
    let limit = 1.10;
    let upper = super::corrected_upper(measured, base_drift, candidate_drift).unwrap();
    assert!(upper > limit);
    assert!(
        measured * (1.0 + candidate_drift) < limit,
        "one-sided correction would falsely pass"
    );
    assert!(super::corrected_upper(1.0, 0.0, 1.0).is_err());
}

#[test]
fn declared_invariant_must_hold_across_engine_arms() {
    super::qualify_control(1.015, true).unwrap();
    super::qualify_control(0.985, true).unwrap();
    assert!(super::qualify_control(1.016, true).is_err());
    assert!(super::qualify_control(0.984, true).is_err());
    // A target phase is judged by the configured acceptance limit, not the control bound.
    super::qualify_control(1.09, false).unwrap();
}

#[test]
fn same_arm_null_compares_first_and_second_positions() {
    let first = row("malloc|plain|EE|0|0", "E", 100);
    let second = row("malloc|plain|EE|0|1", "E", 120);
    let rows = [first, second];
    let by_key = rows
        .iter()
        .map(|row| (row.key.as_str(), row))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        super::paired(&by_key, "malloc", "plain", "EE", "E", "E", "malloc", 1).unwrap(),
        [1.2]
    );
}

#[test]
fn evidence_provenance_must_match_the_scheduled_binary_and_layout() {
    let step = Step {
        workload: "malloc".into(),
        layout: "plain".into(),
        cell: "EE".into(),
        round: 0,
        position: 0,
        arm: "E".into(),
    };
    let valid = row("malloc|plain|EE|0|0", "E", 100);
    super::verify_row_provenance(&step, &valid).unwrap();

    let mut wrong_binary = valid.clone();
    wrong_binary.arm = "R".into();
    assert!(super::verify_row_provenance(&step, &wrong_binary).is_err());

    let mut wrong_layout = valid.clone();
    wrong_layout.layout = "sqlite".into();
    assert!(super::verify_row_provenance(&step, &wrong_layout).is_err());
}

#[test]
fn evidence_phase_coverage_must_be_exact() {
    let expected = ["malloc".to_owned()];
    let complete = row("malloc|plain|EE|0|0", "E", 100);
    super::verify_phase_coverage(&complete, &expected).unwrap();

    let mut missing = complete.clone();
    missing.phases.clear();
    assert!(super::verify_phase_coverage(&missing, &expected).is_err());

    let mut extra = complete;
    extra.phases.insert(
        "unplanned".into(),
        super::super::evidence::Phase {
            us: 1,
            ok: "same".into(),
        },
    );
    assert!(super::verify_phase_coverage(&extra, &expected).is_err());
}

#[test]
fn evidence_duration_must_be_positive() {
    let expected = ["malloc".to_owned()];
    let mut zero = row("malloc|plain|EE|0|0", "E", 0);
    assert!(super::verify_phase_coverage(&zero, &expected).is_err());
    zero.phases.get_mut("malloc").unwrap().us = 1;
    super::verify_phase_coverage(&zero, &expected).unwrap();
}

#[test]
fn host_load_must_be_finite_numeric_evidence() {
    let mut evidence = row("malloc|plain|EE|0|0", "E", 1);
    super::verify_host_load(&evidence, 1).unwrap();
    evidence.host_load[0].after = f64::NAN;
    assert!(super::verify_host_load(&evidence, 1).is_err());
    evidence.host_load[0].after = -0.1;
    assert!(super::verify_host_load(&evidence, 1).is_err());
    evidence.host_load[0].after = 0.2;
    for invalid_count in [0, 2] {
        assert!(super::verify_host_load(&evidence, invalid_count).is_err());
    }
}

#[test]
fn exact_output_identity_must_be_a_canonical_digest() {
    let digest = "0123456789abcdef".repeat(4);
    assert!(super::valid_identity(&digest));
    for invalid in [
        "same".to_owned(),
        "g".repeat(64),
        "A".repeat(64),
        "0".repeat(63),
        "0".repeat(65),
    ] {
        assert!(!super::valid_identity(&invalid), "accepted {invalid:?}");
    }
    let mut evidence = row("malloc|plain|EE|0|0", "E", 1);
    evidence.output = "corrupt".into();
    assert!(super::verify_outputs(&[evidence]).is_err());
}

#[test]
fn exact_output_digest_is_recomputed_from_its_frame() {
    let mut evidence = row("malloc|plain|EE|0|0", "E", 1);
    evidence.output_frame = "META guest=plain".into();
    evidence.output = crate::record::FramedIdentity::of(evidence.output_frame.as_bytes());
    super::verify_outputs(std::slice::from_ref(&evidence)).unwrap();
    evidence.output_frame.push('x');
    assert!(super::verify_outputs(&[evidence]).is_err());
}

#[test]
fn phase_checksums_are_bound_to_the_exact_output_frame() {
    let mut evidence = row("malloc|plain|EE|0|0", "E", 1);
    evidence.output_frame = "META guest=plain\nPHASE malloc us=<time> ok=same".into();
    super::verify_phase_frame(&evidence).unwrap();
    evidence.phases.get_mut("malloc").unwrap().ok = "corrupt".into();
    assert!(super::verify_phase_frame(&evidence).is_err());
    evidence.phases.get_mut("malloc").unwrap().ok = "same".into();
    evidence.output_frame.push_str("\nPHASE extra us=<time> ok=same");
    assert!(super::verify_phase_frame(&evidence).is_err());
}

#[test]
fn crossed_schedule_balance_is_independently_validated() {
    let steps = |first: [&str; 4]| {
        first
            .into_iter()
            .enumerate()
            .flat_map(|(round, first)| {
                let second = if first == "E" { "I" } else { "E" };
                [first, second]
                    .into_iter()
                    .enumerate()
                    .map(move |(position, arm)| Step {
                        workload: "malloc".into(),
                        layout: "plain".into(),
                        cell: "EI".into(),
                        round: round as u32,
                        position,
                        arm: arm.into(),
                    })
            })
            .collect::<Vec<_>>()
    };
    super::verify_balanced_order(&steps(["E", "I", "I", "E"])).unwrap();
    assert!(super::verify_balanced_order(&steps(["E", "E", "E", "E"])).is_err());
    assert!(super::verify_balanced_order(&steps(["E", "I", "E", "I"])).is_err());
}

#[test]
fn campaign_coverage_is_independent_of_the_scheduler() {
    let mut plan = super::CELLS
        .into_iter()
        .flat_map(|(left, right)| {
            (0..4).flat_map(move |round| {
                [left, right].into_iter().enumerate().map(move |(position, arm)| Step {
                    workload: "malloc".into(),
                    layout: "plain".into(),
                    cell: format!("{left}{right}"),
                    round,
                    position,
                    arm: arm.into(),
                })
            })
        })
        .collect::<Vec<_>>();
    super::verify_context_plan(&[("malloc", "plain")], 4, &plan).unwrap();
    plan.pop();
    assert!(super::verify_context_plan(&[("malloc", "plain")], 4, &plan).is_err());
    plan.push(plan[0].clone());
    assert!(super::verify_context_plan(&[("malloc", "plain")], 4, &plan).is_err());
}
