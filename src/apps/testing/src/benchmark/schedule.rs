use super::definition::Campaign;

pub(super) const CELLS: [(&str, &str); 6] = [("E", "E"), ("R", "R"), ("I", "I"), ("E", "R"), ("E", "I"), ("R", "I")];
const ORDER: [[usize; 2]; 4] = [[0, 1], [1, 0], [1, 0], [0, 1]];
const WARMUP_PAIRS: u32 = 4;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct Step {
    pub workload: String,
    pub layout: String,
    pub cell: String,
    pub round: u32,
    pub position: usize,
    pub arm: String,
}

impl Step {
    pub fn key(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}",
            self.workload, self.layout, self.cell, self.round, self.position
        )
    }

    pub fn paired_key(&self) -> Option<String> {
        let position = match self.position {
            0 => 1,
            1 => 0,
            _ => return None,
        };
        Some(format!(
            "{}|{}|{}|{}|{}",
            self.workload, self.layout, self.cell, self.round, position
        ))
    }
}

pub(super) fn warmups(campaign: &Campaign) -> Vec<Step> {
    plan(campaign, WARMUP_PAIRS)
}

pub(super) fn measurements(campaign: &Campaign) -> Vec<Step> {
    plan(campaign, campaign.rounds)
}

fn plan(campaign: &Campaign, rounds: u32) -> Vec<Step> {
    campaign
        .workloads
        .iter()
        .flat_map(|(workload, definition)| {
            definition.commands.keys().flat_map(move |layout| {
                CELLS
                    .into_iter()
                    .flat_map(move |cell| cell_steps(workload, layout, cell, rounds))
            })
        })
        .collect()
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
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{ORDER, Step};

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
        };
        assert_eq!(step.paired_key().as_deref(), Some("malloc|plain|EI|2|1"));
        step.position = 1;
        assert_eq!(step.paired_key().as_deref(), Some("malloc|plain|EI|2|0"));
        step.position = 2;
        assert!(step.paired_key().is_none());
    }
}
