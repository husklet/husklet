use super::{
    definition::Campaign,
    evidence::Row,
    schedule::{self, CELLS, Step},
};
use crate::{record::FramedIdentity, suite::Error};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct Report {
    pub verdict: &'static str,
    pub text: String,
}
impl Report {
    pub(super) fn evaluate(campaign: &Campaign, rows: &[Row], limit: f64) -> Result<Self, Error> {
        if !limit.is_finite() || limit < 1.0 {
            return Err("benchmark limit must be finite and at least 1".into());
        }
        let plan = schedule::measurements(campaign);
        verify_complete_plan(campaign, &plan)?;
        verify_balanced_order(&plan)?;
        verify_plan(campaign, &plan, rows)?;
        verify_outputs(rows)?;
        let by_key = rows
            .iter()
            .map(|row| (row.key.as_str(), row))
            .collect::<BTreeMap<_, _>>();
        let mut nulls = BTreeMap::new();
        let invariant = campaign
            .invariant_phases
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        for (workload, definition) in &campaign.workloads {
            collect_nulls(campaign, &by_key, &invariant, workload, definition, &mut nulls)?;
        }
        let mut verdict = "PASS";
        let mut lines = vec!["workload\tlayout\tcell\tphase\tratio\tnull_floor\tupper\tverdict".to_owned()];
        for (workload, definition) in &campaign.workloads {
            if append_comparisons(campaign, &by_key, &nulls, workload, definition, limit, &mut lines)? {
                verdict = "FAIL";
            }
        }
        lines.push("artifact\tsha256".to_owned());
        for (arm, definition) in &campaign.arms {
            for (name, artifact) in &definition.artifacts {
                lines.push(format!("{arm}/{name}\t{}", artifact.sha256));
            }
        }
        append_compatibility(campaign, &mut lines);
        lines.push(format!("rootfs\t{}", campaign.rootfs.sha256));
        lines.push("sample\trepetition\thost_load_before\thost_load_after".to_owned());
        for row in rows {
            lines.extend(row.host_load_rows());
        }
        lines.push("sample\tarm\tdiagnostic".to_owned());
        for row in rows {
            if let Some(diagnostic) = &row.diagnostic {
                lines.push(format!("{}\t{}\t{}", row.key, row.arm, diagnostic));
            }
        }
        Ok(Self {
            verdict,
            text: lines.join("\n") + "\n",
        })
    }
}

fn append_compatibility(campaign: &Campaign, lines: &mut Vec<String>) {
    lines.push("compatibility\tstate\tstatus\tartifact_sha256\tstderr".to_owned());
    let support = campaign.workloads.iter().flat_map(|(workload, definition)| {
        definition
            .arm_support
            .iter()
            .flat_map(move |(layout, support)| support.iter().map(move |(arm, state)| (workload, layout, arm, state)))
    });
    for (workload, layout, arm, state) in support {
        let super::definition::ArmSupport::Incompatible {
            status,
            stderr,
            artifact_sha256,
        } = state
        else {
            continue;
        };
        lines.push(format!(
            "{workload}/{layout}/{arm}\tincompatible\t{status}\t{artifact_sha256}\t{}",
            stderr.replace(['\r', '\n', '\t'], " ")
        ));
    }
}

use verify::{NullKey, phases, verify_balanced_order, verify_complete_plan, verify_plan};
#[cfg(test)]
use verify::{verify_context_plan, verify_host_load, verify_phase_coverage, verify_phase_frame, verify_row_provenance};

#[path = "verdict/verify.rs"]
mod verify;

fn collect_nulls<'a>(
    campaign: &'a Campaign,
    rows: &BTreeMap<&str, &Row>,
    invariant: &BTreeSet<&str>,
    workload: &'a str,
    definition: &'a super::definition::Workload,
    nulls: &mut BTreeMap<NullKey<'a>, f64>,
) -> Result<(), Error> {
    for layout in definition.commands.keys() {
        collect_layout_nulls(campaign, rows, invariant, workload, layout, nulls)?;
    }
    Ok(())
}

fn collect_layout_nulls<'a>(
    campaign: &'a Campaign,
    rows: &BTreeMap<&str, &Row>,
    invariant: &BTreeSet<&str>,
    workload: &'a str,
    layout: &'a str,
    nulls: &mut BTreeMap<NullKey<'a>, f64>,
) -> Result<(), Error> {
    for arm in ["E", "R", "I"]
        .into_iter()
        .filter(|arm| campaign.workloads[workload].arm_support[layout][*arm].available())
    {
        let cell = format!("{arm}{arm}");
        for phase in phases(campaign, workload, layout) {
            let ratios = paired(rows, workload, layout, &cell, arm, arm, phase, campaign.rounds)?;
            nulls.insert(
                (workload, layout, arm, phase),
                qualify_null(&ratios, invariant.contains(phase.as_str()))?,
            );
        }
    }
    Ok(())
}

fn append_comparisons(
    campaign: &Campaign,
    rows: &BTreeMap<&str, &Row>,
    nulls: &BTreeMap<NullKey<'_>, f64>,
    workload: &str,
    definition: &super::definition::Workload,
    limit: f64,
    lines: &mut Vec<String>,
) -> Result<bool, Error> {
    let mut failed = false;
    for layout in definition.commands.keys() {
        for &(left, right) in &CELLS[3..] {
            let support = &definition.arm_support[layout];
            if !support[left].available() || !support[right].available() {
                continue;
            }
            failed |= append_cell(campaign, rows, nulls, workload, layout, left, right, limit, lines)?;
        }
    }
    Ok(failed)
}

fn append_cell(
    campaign: &Campaign,
    rows: &BTreeMap<&str, &Row>,
    nulls: &BTreeMap<NullKey<'_>, f64>,
    workload: &str,
    layout: &str,
    left: &str,
    right: &str,
    limit: f64,
    lines: &mut Vec<String>,
) -> Result<bool, Error> {
    let cell = format!("{left}{right}");
    let mut failed = false;
    for phase in phases(campaign, workload, layout) {
        let mut ratios = paired(rows, workload, layout, &cell, left, right, phase, campaign.rounds)?;
        let ratio = median(&mut ratios);
        let phase_name = phase.as_str();
        let left_floor = nulls[&(workload, layout, left, phase_name)];
        let right_floor = nulls[&(workload, layout, right, phase_name)];
        let floor = left_floor.max(right_floor);
        let upper = corrected_upper(ratio, left_floor, right_floor)?;
        qualify_control(ratio, campaign.invariant_phases.iter().any(|item| item == phase))?;
        let judged = right == "I" && campaign.workloads[workload].phases.iter().any(|item| item == phase);
        let result = if judged && upper > limit {
            failed = true;
            "FAIL"
        } else if judged {
            "PASS"
        } else {
            "INFO"
        };
        lines.push(format!(
            "{workload}\t{layout}\t{cell}\t{phase}\t{ratio:.6}\t{floor:.6}\t{upper:.6}\t{result}"
        ));
    }
    Ok(failed)
}

fn qualify_control(ratio: f64, invariant: bool) -> Result<(), Error> {
    if !ratio.is_finite() || ratio <= 0.0 {
        return Err("invalid invariant-control ratio".into());
    }
    if invariant && !(0.985..=1.015).contains(&ratio) {
        return Err(format!("unqualified invariant control: ratio={ratio:.6}").into());
    }
    Ok(())
}

fn corrected_upper(ratio: f64, left_floor: f64, right_floor: f64) -> Result<f64, Error> {
    if !ratio.is_finite()
        || ratio < 0.0
        || !left_floor.is_finite()
        || !(0.0..1.0).contains(&left_floor)
        || !right_floor.is_finite()
        || !(0.0..1.0).contains(&right_floor)
    {
        return Err("invalid ratio or null floor in benchmark correction".into());
    }
    // The crossed ratio is smallest when the left sample is high by its null floor and
    // the right sample is low by its null floor. Undo both directions before judging it.
    Ok(ratio * (1.0 + left_floor) / (1.0 - right_floor))
}

fn verify_outputs(rows: &[Row]) -> Result<(), Error> {
    let mut expected = BTreeMap::new();
    for row in rows {
        if !valid_identity(&row.output) {
            return Err(format!("invalid exact-output identity for {}", row.key).into());
        }
        if FramedIdentity::of(row.output_frame.as_bytes()) != row.output {
            return Err(format!("exact-output frame identity differs for {}", row.key).into());
        }
        let value = (
            row.output.as_str(),
            row.output_frame.as_str(),
            row.phases
                .iter()
                .map(|(name, phase)| (name.as_str(), phase.ok.as_str()))
                .collect::<Vec<_>>(),
        );
        let key = (row.workload.as_str(), row.layout.as_str());
        if expected.insert(key, value.clone()).is_some_and(|prior| prior != value) {
            return Err(format!("exact-output mismatch for {}/{}", row.workload, row.layout).into());
        }
    }
    Ok(())
}

fn valid_identity(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn paired(
    by_key: &BTreeMap<&str, &Row>,
    workload: &str,
    layout: &str,
    cell: &str,
    left: &str,
    right: &str,
    phase: &str,
    rounds: u32,
) -> Result<Vec<f64>, Error> {
    let mut values = Vec::new();
    for round in 0..rounds {
        let first = by_key
            .get(format!("{workload}|{layout}|{cell}|{round}|0").as_str())
            .ok_or("missing first paired row")?;
        let second = by_key
            .get(format!("{workload}|{layout}|{cell}|{round}|1").as_str())
            .ok_or("missing second paired row")?;
        let (a, b) = if left == right {
            (
                first.phases.get(phase).ok_or("pair omitted phase")?.us,
                second.phases.get(phase).ok_or("pair omitted phase")?.us,
            )
        } else {
            let samples = [(first.arm.as_str(), first), (second.arm.as_str(), second)]
                .into_iter()
                .collect::<BTreeMap<_, _>>();
            (
                samples
                    .get(left)
                    .ok_or("pair omitted left arm")?
                    .phases
                    .get(phase)
                    .ok_or("pair omitted phase")?
                    .us,
                samples
                    .get(right)
                    .ok_or("pair omitted right arm")?
                    .phases
                    .get(phase)
                    .ok_or("pair omitted phase")?
                    .us,
            )
        };
        values.push(b as f64 / a.max(1) as f64);
    }
    Ok(values)
}

fn qualify_null(values: &[f64], invariant: bool) -> Result<f64, Error> {
    if values.len() < 4 {
        return Err("null has fewer than four paired samples".into());
    }
    let mut all = values.to_vec();
    let center = median(&mut all);
    // ORDER is AB, BA, BA, AB. Compare like-order rounds rather than parity:
    // parity would mix one AB and one BA round into each purported order stratum.
    let mut forward = Vec::new();
    let mut reverse = Vec::new();
    for (round, value) in values.iter().copied().enumerate() {
        if round % 4 == 0 || round % 4 == 3 {
            forward.push(value);
        } else {
            reverse.push(value);
        }
    }
    let middle = values.len() / 2;
    let mut early = values[..middle].to_vec();
    let mut late = values[middle..].to_vec();
    let floor = values.iter().map(|value| (value - 1.0).abs()).fold(0.0_f64, f64::max);
    if (center - 1.0).abs() > 0.01 {
        return Err("unqualified null: center".into());
    }
    if [median(&mut forward), median(&mut reverse)]
        .into_iter()
        .any(|value| (value - 1.0).abs() > 0.01)
    {
        return Err("unqualified null: order strata".into());
    }
    if [median(&mut early), median(&mut late)]
        .into_iter()
        .any(|value| (value - 1.0).abs() > 0.01)
    {
        return Err("unqualified null: temporal strata".into());
    }
    if values.iter().any(|value| (value - 1.0).abs() > 0.05) {
        return Err("unqualified null: individual pair".into());
    }
    if floor > if invariant { 0.015 } else { 0.03 } {
        return Err(format!(
            "unqualified null: {} floor",
            if invariant { "invariant" } else { "null" }
        )
        .into());
    }
    Ok(floor)
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        f64::midpoint(values[middle - 1], values[middle])
    } else {
        values[middle]
    }
}

#[cfg(test)]
#[path = "verdict/test.rs"]
mod tests;
