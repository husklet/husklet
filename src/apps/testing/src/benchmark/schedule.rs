use super::definition::{ArmSupport, Campaign, ProfileKind};
use std::collections::{BTreeMap, BTreeSet};

pub(super) const CELLS: [(&str, &str); 3] = [("E", "R"), ("E", "I"), ("R", "I")];
const ORDER: [[usize; 2]; 4] = [[0, 1], [1, 0], [1, 0], [0, 1]];
const WARMUP_INVOCATIONS: usize = 6;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct Step {
    pub workload: String,
    pub layout: String,
    pub cell: String,
    pub round: u32,
    pub position: usize,
    pub arm: String,
    pub profile: ProfileKind,
    pub paired_profile: ProfileKind,
}

impl Step {
    pub fn key(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}",
            self.workload,
            self.layout,
            self.cell,
            self.profile.as_str(),
            self.round,
            self.position
        )
    }

    pub fn paired_key(&self) -> Option<String> {
        let position = match self.position {
            0 => 1,
            1 => 0,
            _ => return None,
        };
        Some(format!(
            "{}|{}|{}|{}|{}|{}",
            self.workload,
            self.layout,
            self.cell,
            self.paired_profile.as_str(),
            self.round,
            position
        ))
    }
}

/// Fixed, timing-independent warmups immediately preceding a cell's first measured pair.
pub(super) fn pair_warmups(pair: &[Step]) -> Vec<Step> {
    let mut profiles = pair
        .iter()
        .map(|step| (step.arm.as_str(), step.profile))
        .collect::<Vec<_>>();
    profiles.sort_unstable();
    profiles.dedup();
    profiles
        .into_iter()
        .flat_map(|(arm, profile)| {
            (0..WARMUP_INVOCATIONS).map(move |_| Step {
                workload: pair[0].workload.clone(),
                layout: pair[0].layout.clone(),
                cell: pair[0].cell.clone(),
                round: pair[0].round,
                position: 0,
                arm: arm.to_owned(),
                profile,
                paired_profile: profile,
            })
        })
        .collect()
}

pub(super) fn warmups_for_first_missing(
    warmed: &mut BTreeSet<(String, String, String)>,
    pair: &[Step],
    already_complete: bool,
) -> Vec<Step> {
    if already_complete || pair.is_empty() {
        return Vec::new();
    }
    let context = (pair[0].workload.clone(), pair[0].layout.clone(), pair[0].cell.clone());
    if warmed.insert(context) {
        pair_warmups(pair)
    } else {
        Vec::new()
    }
}

pub(super) fn measurements(campaign: &Campaign) -> Vec<Step> {
    plan(campaign, campaign.rounds)
        .into_iter()
        .chain(independent_nulls(campaign, campaign.rounds))
        .collect()
}

pub(super) fn calibration(campaign: &Campaign, arms: &[String], rounds: u32) -> Vec<Step> {
    campaign
        .workloads
        .iter()
        .flat_map(|(workload, definition)| {
            definition.commands.keys().flat_map(move |layout| {
                arms.iter()
                    .filter(move |arm| {
                        definition.arm_support[layout]
                            .get(*arm)
                            .is_some_and(ArmSupport::available)
                    })
                    .map(move |arm| cell_steps_owned(workload, layout, arm, rounds))
            })
        })
        .flatten()
        .collect()
}

fn cell_steps_owned(workload: &str, layout: &str, arm: &str, rounds: u32) -> Vec<Step> {
    (0..rounds)
        .flat_map(|round| {
            ORDER[round as usize % ORDER.len()]
                .into_iter()
                .enumerate()
                .map(move |(position, _)| Step {
                    workload: workload.to_owned(),
                    layout: layout.to_owned(),
                    cell: format!("{arm}{arm}"),
                    round,
                    position,
                    arm: arm.to_owned(),
                    profile: ProfileKind::Primary,
                    paired_profile: ProfileKind::Primary,
                })
        })
        .collect()
}

fn plan(campaign: &Campaign, rounds: u32) -> Vec<Step> {
    campaign
        .workloads
        .iter()
        .flat_map(|(workload, definition)| {
            definition.commands.keys().flat_map(move |layout| {
                let support = &definition.arm_support[layout];
                supported_cells(support).flat_map(move |cell| cell_steps(workload, layout, cell, rounds))
            })
        })
        .collect()
}

fn supported_cells(support: &BTreeMap<String, ArmSupport>) -> impl Iterator<Item = (&'static str, &'static str)> + '_ {
    CELLS
        .into_iter()
        .filter(|(left, right)| support[*left].available() && support[*right].available())
}

fn cell_steps(workload: &str, layout: &str, (left, right): (&str, &str), rounds: u32) -> Vec<Step> {
    let arms = [left, right];
    (0..rounds)
        .flat_map(|round| {
            ORDER[round as usize % ORDER.len()]
                .into_iter()
                .enumerate()
                .map(move |(position, index)| Step {
                    workload: workload.to_owned(),
                    layout: layout.to_owned(),
                    cell: format!("{left}{right}"),
                    round,
                    position,
                    arm: arms[index].to_owned(),
                    profile: ProfileKind::Primary,
                    paired_profile: ProfileKind::Primary,
                })
        })
        .collect()
}

fn independent_nulls(campaign: &Campaign, rounds: u32) -> Vec<Step> {
    campaign
        .workloads
        .iter()
        .flat_map(|(workload, definition)| {
            definition.commands.keys().flat_map(move |layout| {
                ["E", "I"].into_iter().flat_map(move |arm| {
                    let available = definition.arm_support[layout][arm].available()
                        && campaign.arms[arm].independent_null.is_some();
                    available
                        .then(|| null_steps(workload, layout, arm, rounds))
                        .into_iter()
                        .flatten()
                })
            })
        })
        .collect()
}

fn null_steps(workload: &str, layout: &str, arm: &str, rounds: u32) -> Vec<Step> {
    let profiles = [ProfileKind::Primary, ProfileKind::IndependentNull];
    (0..rounds)
        .flat_map(|round| {
            ORDER[round as usize % ORDER.len()]
                .into_iter()
                .enumerate()
                .map(move |(position, index)| Step {
                    workload: workload.to_owned(),
                    layout: layout.to_owned(),
                    cell: format!("{arm}{arm}"),
                    round,
                    position,
                    arm: arm.to_owned(),
                    profile: profiles[index],
                    paired_profile: profiles[1 - index],
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{ArmSupport, BTreeMap, ORDER, ProfileKind, Step};

    #[test]
    fn crossed_order_balances_position_and_temporal_strata() {
        assert_eq!(ORDER, [[0, 1], [1, 0], [1, 0], [0, 1]]);
        assert_eq!(ORDER.iter().filter(|pair| pair[0] == 0).count(), 2);
    }

    #[test]
    fn pair_identity_exchanges_only_the_two_scheduled_positions() {
        let mut step = Step {
            workload: "malloc".into(),
            layout: "plain".into(),
            cell: "EI".into(),
            round: 2,
            position: 0,
            arm: "E".into(),
            profile: ProfileKind::Primary,
            paired_profile: ProfileKind::Primary,
        };
        assert_eq!(step.paired_key().as_deref(), Some("malloc|plain|EI|primary|2|1"));
        step.position = 1;
        assert_eq!(step.paired_key().as_deref(), Some("malloc|plain|EI|primary|2|0"));
        step.position = 2;
        assert!(step.paired_key().is_none());
    }

    #[test]
    fn classified_retained_failure_removes_only_retained_cells() {
        let support = BTreeMap::from([
            ("E".into(), ArmSupport::Available),
            ("I".into(), ArmSupport::Available),
            (
                "R".into(),
                ArmSupport::Incompatible {
                    status: 1,
                    stderr: "failure".into(),
                    artifact_sha256: "a".repeat(64),
                },
            ),
        ]);
        assert_eq!(super::supported_cells(&support).collect::<Vec<_>>(), [("E", "I")]);
    }

    #[test]
    fn calibration_contains_only_balanced_same_arm_pairs() {
        let steps = super::cell_steps_owned("malloc", "plain", "E", 12);
        assert_eq!(steps.len(), 24);
        assert!(steps.iter().all(|step| step.cell == "EE" && step.arm == "E"));
        for pair in steps.chunks_exact(2) {
            assert_eq!(pair[0].round, pair[1].round);
            assert_eq!([pair[0].position, pair[1].position], [0, 1]);
        }
    }

    #[test]
    fn independent_null_exchanges_build_profiles_in_balanced_order() {
        let steps = super::null_steps("malloc", "plain", "E", 4);
        assert_eq!(steps.len(), 8);
        for pair in steps.chunks_exact(2) {
            assert_eq!(pair[0].paired_key().as_deref(), Some(pair[1].key().as_str()));
            assert_eq!(pair[1].paired_key().as_deref(), Some(pair[0].key().as_str()));
            assert_ne!(pair[0].profile, pair[1].profile);
        }
        assert_eq!(
            steps
                .iter()
                .filter(|step| step.position == 0 && step.profile == ProfileKind::Primary)
                .count(),
            2
        );
        assert!(steps.iter().all(|step| step.cell == "EE" && step.arm == "E"));
    }

    #[test]
    fn just_in_time_warmups_are_fixed_and_cover_each_distinct_arm() {
        let pair = super::cell_steps("malloc", "sqlite", ("E", "I"), 1);
        let warmups = super::pair_warmups(&pair);
        assert_eq!(warmups.len(), 12);
        assert_eq!(warmups.iter().filter(|step| step.arm == "E").count(), 6);
        assert_eq!(warmups.iter().filter(|step| step.arm == "I").count(), 6);
        assert!(
            warmups
                .iter()
                .all(|step| step.workload == "malloc" && step.layout == "sqlite")
        );
    }

    #[test]
    fn resume_warms_immediately_before_each_contexts_first_missing_pair() {
        let first = super::cell_steps("malloc", "plain", ("E", "E"), 2);
        let second = super::cell_steps("malloc", "sqlite", ("E", "E"), 1);
        let mut warmed = std::collections::BTreeSet::new();
        assert!(super::warmups_for_first_missing(&mut warmed, &first[..2], true).is_empty());
        assert_eq!(
            super::warmups_for_first_missing(&mut warmed, &first[2..4], false).len(),
            6
        );
        assert!(super::warmups_for_first_missing(&mut warmed, &first[..2], false).is_empty());
        assert_eq!(
            super::warmups_for_first_missing(&mut warmed, &second[..2], false).len(),
            6
        );
    }
}
