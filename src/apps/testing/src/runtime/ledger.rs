use super::WorkKey;
use super::diagnostic::BoundedDiagnostic as _;
use crate::{
    journal::{self, Attempt, Require as _, Schema},
    suite::{Error, Target},
};
use std::collections::{BTreeMap, BTreeSet};

pub(super) type Ledger = journal::Ledger<Runtime>;

pub(super) const PASS: &str = "pass";
pub(super) const FAIL: &str = "fail";
/// A case the sweep never attempted: deliberately inactive, or lost to an abort.
pub(super) const NOT_RUN: &str = "NOT_RUN";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Row {
    pub attempt: Attempt<WorkKey>,
    /// Host load when the row was recorded, so a contended run is distinguishable from a real timeout.
    pub host_load: String,
    pub diagnostic: String,
    pub campaign: CampaignEvidence,
}

/// Portable, content-bound evidence for one arm of a cross-ISA measurement pair.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CampaignEvidence {
    pub host_identity: String,
    pub artifact_sha256: String,
    pub wall_ns: u64,
    pub task_clock_ns: u64,
    pub instructions: u64,
    pub cycles: u64,
    pub faults: u64,
    pub semantic_output_sha256: String,
    pub backend_digest: String,
    pub pair: String,
    pub arm: String,
    pub order: u8,
    pub sample: u32,
}

impl CampaignEvidence {
    pub(super) fn unmeasured() -> Self {
        Self {
            host_identity: "-".into(), artifact_sha256: "-".into(), wall_ns: 0, task_clock_ns: 0,
            instructions: 0, cycles: 0, faults: 0, semantic_output_sha256: "-".into(),
            backend_digest: "-".into(), pair: "-".into(), arm: "-".into(), order: 0, sample: 0,
        }
    }

    fn validate(&self) -> Result<(), Error> {
        let safe = |value: &str| !value.is_empty() && !value.contains(['\t', '\n']);
        let sha256 = |value: &str| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit());
        [self.host_identity.as_str(), self.artifact_sha256.as_str(), self.semantic_output_sha256.as_str(),
         self.backend_digest.as_str(), self.pair.as_str(), self.arm.as_str()]
            .into_iter().all(safe).require("runtime campaign evidence contains an unsafe field")?;
        if self.pair == "-" {
            (self.arm == "-" && self.order == 0 && self.sample == 0)
                .require("unpaired runtime row carries pair coordinates")?;
        } else {
            (sha256(&self.host_identity) && sha256(&self.artifact_sha256)
                && sha256(&self.semantic_output_sha256) && self.backend_digest != "-"
                && !self.arm.is_empty() && (1..=2).contains(&self.order) && self.sample > 0)
                .require("paired runtime row has incomplete immutable evidence")?;
        }
        Ok(())
    }
}

/// The result schema of a runtime compatibility run.
pub(super) struct Runtime;

impl Schema for Runtime {
    type Key = WorkKey;
    type Row = Row;

    const KIND: &'static str = "runtime";
    const HEADER: &'static str = "id\ttarget\tprofile\tstatus\telapsed_ms\thost_load\thost_identity\tartifact_sha256\twall_ns\ttask_clock_ns\tinstructions\tcycles\tfaults\tsemantic_output_sha256\tbackend_digest\tpair\tarm\torder\tsample\tdiagnostic\n";
    const ROW_LIMIT: usize = 16 * 1024;
    const FIELDS: usize = 20;

    fn key(row: &Row) -> &WorkKey {
        &row.attempt.key
    }

    /// Never fails on diagnostic shape: a truncated row is worth more than an aborted sweep.
    fn format(row: &Row) -> Result<String, Error> {
        row.campaign.validate()?;
        (!row.attempt.key.id.contains(['\t', '\n'])).require("runtime result contains an unsafe delimiter")?;
        let load = super::load::sanitize(&row.host_load);
        let prefix = format!(
            "{}\t{}\t{}\t{}\t{}\t{load}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t",
            row.attempt.key.id,
            row.attempt.key.target.name(),
            super::profile::PROFILE,
            row.attempt.status,
            row.attempt.elapsed_ms, row.campaign.host_identity, row.campaign.artifact_sha256,
            row.campaign.wall_ns, row.campaign.task_clock_ns, row.campaign.instructions, row.campaign.cycles,
            row.campaign.faults, row.campaign.semantic_output_sha256, row.campaign.backend_digest,
            row.campaign.pair, row.campaign.arm, row.campaign.order, row.campaign.sample
        );
        let diagnostic = row.diagnostic.replace(['\t', '\n'], " ");
        let room = Self::ROW_LIMIT.saturating_sub(prefix.len() + 1);
        (room > 64).require("runtime result key exceeds its byte bound")?;
        Ok(format!("{prefix}{}\n", diagnostic.bounded_to(room)))
    }

    fn parse(fields: &[&str], keys: &BTreeSet<WorkKey>) -> Result<Option<Row>, Error> {
        let key = WorkKey {
            id: fields[0].to_owned(),
            target: Target::named(fields[1]).ok_or("invalid runtime resume target")?,
        };
        keys.contains(&key).require("stale runtime resume row")?;
        (fields[2] == super::profile::PROFILE).require("runtime resume row measured another engine profile")?;
        let status = match fields[3] {
            "pass" => PASS,
            "fail" => FAIL,
            NOT_RUN => NOT_RUN,
            _ => return Err("invalid runtime resume status".into()),
        };
        Ok(Some(Row {
            attempt: Attempt {
                key,
                status,
                elapsed_ms: fields[4].parse()?,
            },
            host_load: fields[5].to_owned(),
            campaign: CampaignEvidence {
                host_identity: fields[6].into(), artifact_sha256: fields[7].into(), wall_ns: fields[8].parse()?,
                task_clock_ns: fields[9].parse()?, instructions: fields[10].parse()?, cycles: fields[11].parse()?,
                faults: fields[12].parse()?, semantic_output_sha256: fields[13].into(), backend_digest: fields[14].into(),
                pair: fields[15].into(), arm: fields[16].into(), order: fields[17].parse()?, sample: fields[18].parse()?,
            },
            diagnostic: fields[19].to_owned(),
        }))
    }

    fn validate_complete(rows: &BTreeMap<WorkKey, Row>) -> Result<(), Error> {
        let mut pairs: BTreeMap<(&str, u32), Vec<&CampaignEvidence>> = BTreeMap::new();
        let mut host_identity = None;
        for row in rows.values() {
            row.campaign.validate()?;
            if row.campaign.pair != "-" {
                match host_identity {
                    Some(identity) => (identity == row.campaign.host_identity)
                        .require("runtime campaign journal mixes host identities")?,
                    None => host_identity = Some(row.campaign.host_identity.as_str()),
                }
                pairs.entry((&row.campaign.pair, row.campaign.sample)).or_default().push(&row.campaign);
            }
        }
        for evidence in pairs.values() {
            (evidence.len() == 2).require("runtime campaign contains an incomplete pair")?;
            let first = evidence[0]; let second = evidence[1];
            (first.host_identity == second.host_identity
                && first.semantic_output_sha256 == second.semantic_output_sha256 && first.arm != second.arm
                && [first.order, second.order].into_iter().collect::<BTreeSet<_>>() == BTreeSet::from([1, 2]))
                .require("runtime campaign pair evidence does not match")?;
        }
        Ok(())
    }


    fn validate_resumption(rows: &BTreeMap<WorkKey, Row>) -> Result<(), Error> {
        let mut identity = None;
        for row in rows.values() {
            row.campaign.validate()?;
            if row.campaign.pair != "-" {
                match identity {
                    Some(expected) => (expected == row.campaign.host_identity)
                        .require("runtime campaign resume mixes host identities")?,
                    None => identity = Some(row.campaign.host_identity.as_str()),
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Attempt, CampaignEvidence, Ledger, Row, WorkKey};
    use crate::suite::Target;
    use std::collections::{BTreeMap, BTreeSet};
    use std::io::Write;

    fn key(id: &str) -> WorkKey {
        WorkKey {
            id: id.to_owned(),
            target: Target::Arm64,
        }
    }

    #[test]
    fn durable_rows_resume_and_finish_in_key_order() {
        let directory = tempfile::tempdir().unwrap();
        let report = directory.path().join("results.tsv");
        let keys = BTreeSet::from([key("runtime/a"), key("runtime/b")]);
        let opened = Ledger::open(&report, "stamp", &keys, false).unwrap();
        opened
            .ledger
            .record(Row {
                attempt: Attempt {
                    key: key("runtime/b"),
                    status: "pass",
                    elapsed_ms: 2,
                },
                host_load: "0.20/8".to_owned(),
                diagnostic: String::new(),
                campaign: CampaignEvidence::unmeasured(),
            })
            .unwrap();
        drop(opened);
        let resumed = Ledger::open(&report, "stamp", &keys, true).unwrap();
        assert_eq!(resumed.prior.len(), 1);
        resumed
            .ledger
            .record(Row {
                attempt: Attempt {
                    key: key("runtime/a"),
                    status: "pass",
                    elapsed_ms: 1,
                },
                host_load: "0.10/8".to_owned(),
                diagnostic: String::new(),
                campaign: CampaignEvidence::unmeasured(),
            })
            .unwrap();
        resumed.ledger.finish().unwrap();
        let text = std::fs::read_to_string(report).unwrap();
        assert!(text.find("runtime/a").unwrap() < text.find("runtime/b").unwrap());
    }

    #[test]
    fn an_oversized_diagnostic_is_truncated_rather_than_rejected() {
        use crate::journal::Schema as _;

        let row = Row {
            attempt: Attempt {
                key: key("runtime/loud"),
                status: super::FAIL,
                elapsed_ms: 1,
            },
            host_load: "9.00/8".to_owned(),
            diagnostic: "x\ty\n".repeat(1 << 16),
            campaign: CampaignEvidence::unmeasured(),
        };
        let text = super::Runtime::format(&row).unwrap();
        assert!(text.len() <= super::Runtime::ROW_LIMIT, "{}", text.len());
        assert_eq!(text.matches('\t').count(), 19);
        assert!(text.contains(super::super::profile::PROFILE), "{text}");
        assert!(text.ends_with("truncated]\n"), "{text}");
    }

    #[test]
    fn not_run_rows_round_trip_through_resume() {
        let directory = tempfile::tempdir().unwrap();
        let report = directory.path().join("results.tsv");
        let keys = BTreeSet::from([key("runtime/a")]);
        let opened = Ledger::open(&report, "stamp", &keys, false).unwrap();
        opened
            .ledger
            .record(Row {
                attempt: Attempt {
                    key: key("runtime/a"),
                    status: super::NOT_RUN,
                    elapsed_ms: 0,
                },
                host_load: super::super::load::unmeasured(),
                diagnostic: "BROKEN: retained".to_owned(),
                campaign: CampaignEvidence::unmeasured(),
            })
            .unwrap();
        drop(opened);
        let resumed = Ledger::open(&report, "stamp", &keys, true).unwrap();
        assert_eq!(resumed.prior[&key("runtime/a")].attempt.status, super::NOT_RUN);
        assert_eq!(resumed.ledger.planned(), &keys);
    }

    #[test]
    fn torn_tail_is_dropped_and_stale_stamp_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let report = directory.path().join("results.tsv");
        let keys = BTreeSet::from([key("runtime/a")]);
        let opened = Ledger::open(&report, "stamp", &keys, false).unwrap();
        drop(opened);
        let partial = report.with_extension("partial.tsv");
        let mut file = std::fs::OpenOptions::new().append(true).open(&partial).unwrap();
        file.write_all(b"runtime/a\tarm").unwrap();
        file.sync_data().unwrap();
        drop(file);
        assert!(Ledger::open(&report, "stamp", &keys, true).is_ok());
        assert!(Ledger::open(&report, "changed", &keys, true).is_err());
    }

    fn measured(pair: &str, arm: &str, order: u8) -> CampaignEvidence {
        CampaignEvidence {
            host_identity: "c".repeat(64),
            artifact_sha256: "a".repeat(64),
            wall_ns: 10,
            task_clock_ns: 9,
            instructions: 8,
            cycles: 7,
            faults: 6,
            semantic_output_sha256: "b".repeat(64),
            backend_digest: "backend-tree crossings=1".into(),
            pair: pair.into(),
            arm: arm.into(),
            order,
            sample: 1,
        }
    }

    fn campaign_row(id: &str, evidence: CampaignEvidence) -> Row {
        Row {
            attempt: Attempt { key: key(id), status: super::PASS, elapsed_ms: 1 },
            host_load: "0.10/8".into(),
            diagnostic: String::new(),
            campaign: evidence,
        }
    }

    #[test]
    fn complete_pairs_publish_and_round_trip_every_measurement_field() {
        use crate::journal::Schema as _;
        let rows = BTreeMap::from([
            (key("runtime/a"), campaign_row("runtime/a", measured("p", "baseline", 1))),
            (key("runtime/b"), campaign_row("runtime/b", measured("p", "candidate", 2))),
        ]);
        super::Runtime::validate_complete(&rows).unwrap();
        let text = super::Runtime::format(&rows[&key("runtime/a")]).unwrap();
        assert!(text.contains("\t10\t9\t8\t7\t6\t"), "{text}");
        assert_eq!(text.matches('\t').count(), 19);
        let fields = text.trim_end().split('\t').collect::<Vec<_>>();
        let parsed = super::Runtime::parse(&fields, &BTreeSet::from([key("runtime/a")]))
            .unwrap().unwrap();
        assert_eq!(parsed.campaign, measured("p", "baseline", 1));
    }

    #[test]
    fn incomplete_and_mismatched_pairs_are_refused() {
        use crate::journal::Schema as _;
        let one = campaign_row("runtime/a", measured("p", "baseline", 1));
        assert!(super::Runtime::validate_complete(&BTreeMap::from([(key("runtime/a"), one.clone())])).is_err());
        let mut other = measured("p", "candidate", 2);
        other.semantic_output_sha256 = "c".repeat(64);
        assert!(super::Runtime::validate_complete(&BTreeMap::from([
            (key("runtime/a"), one),
            (key("runtime/b"), campaign_row("runtime/b", other)),
        ])).is_err());
    }

    #[test]
    fn separate_pairs_cannot_resume_across_host_identities() {
        use crate::journal::Schema as _;
        let mut second_host = measured("q", "baseline", 1);
        second_host.host_identity = "d".repeat(64);
        let mut second_host_peer = measured("q", "candidate", 2);
        second_host_peer.host_identity = "d".repeat(64);
        let rows = BTreeMap::from([
            (key("runtime/a"), campaign_row("runtime/a", measured("p", "baseline", 1))),
            (key("runtime/b"), campaign_row("runtime/b", measured("p", "candidate", 2))),
            (key("runtime/c"), campaign_row("runtime/c", second_host)),
            (key("runtime/d"), campaign_row("runtime/d", second_host_peer)),
        ]);
        assert!(super::Runtime::validate_complete(&rows).is_err());
    }

    #[test]
    fn sentinels_and_unsafe_fields_cannot_masquerade_as_measurements() {
        use crate::journal::Schema as _;
        let mut sentinel = CampaignEvidence::unmeasured();
        sentinel.pair = "p".into();
        sentinel.arm = "baseline".into();
        sentinel.order = 1;
        sentinel.sample = 1;
        assert!(super::Runtime::format(&campaign_row("runtime/a", sentinel)).is_err());

        let mut unsafe_evidence = measured("p", "baseline", 1);
        unsafe_evidence.backend_digest = "backend\nforged".into();
        assert!(super::Runtime::format(&campaign_row("runtime/a", unsafe_evidence)).is_err());
    }
}
