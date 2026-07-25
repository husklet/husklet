//! One-to-one provenance for tests embedded in the original container crates.

use std::collections::{BTreeMap, BTreeSet};

const INVENTORY: &str = include_str!("golden/provenance.tests");
const IMAGE_MANIFEST: &str = include_str!("golden/images.manifest");
const SCENARIOS: &str = include_str!("golden/scenario.contracts");
const EXPECTED: usize = 536;
const EXPECTED_IMAGES: usize = 133;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Disposition {
    Mapped,
    Transferred,
    Superseded,
    Pending,
}

impl Disposition {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "mapped" => Ok(Self::Mapped),
            "transferred" => Ok(Self::Transferred),
            "superseded" => Ok(Self::Superseded),
            "pending" => Ok(Self::Pending),
            _ => Err(format!("unknown provenance disposition {value:?}")),
        }
    }
}

pub fn audit(strict: bool) -> Result<(), String> {
    audit_images()?;
    let mut identities = BTreeSet::new();
    let mut counts = BTreeMap::<Disposition, usize>::new();
    let mut pending = Vec::new();

    for (index, line) in INVENTORY.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 5 {
            return Err(format!(
                "provenance.tests:{} has {} fields; expected five",
                index + 1,
                fields.len()
            ));
        }
        validate_owner(fields[0], fields[1])
            .map_err(|error| format!("provenance.tests:{}: {error}", index + 1))?;
        let identity = format!("{}::{}", fields[1], fields[2]);
        if !identities.insert(identity.clone()) {
            return Err(format!("duplicate provenance test identity {identity:?}"));
        }
        let disposition = Disposition::parse(fields[3])?;
        if disposition != Disposition::Pending && fields[4] == "-" {
            return Err(format!(
                "resolved provenance test {identity:?} has no proof"
            ));
        }
        if disposition == Disposition::Pending {
            pending.push(identity);
        }
        *counts.entry(disposition).or_default() += 1;
    }

    if identities.len() != EXPECTED {
        return Err(format!(
            "provenance test inventory has {} identities; expected {EXPECTED}",
            identities.len()
        ));
    }
    println!(
        "provenance tests: total={EXPECTED} mapped={} transferred={} superseded={} pending={}",
        counts
            .get(&Disposition::Mapped)
            .copied()
            .unwrap_or_default(),
        counts
            .get(&Disposition::Transferred)
            .copied()
            .unwrap_or_default(),
        counts
            .get(&Disposition::Superseded)
            .copied()
            .unwrap_or_default(),
        pending.len(),
    );
    if strict && !pending.is_empty() {
        return Err(format!(
            "{} historical Rust tests lack one-to-one behavioral proof; first pending: {}",
            pending.len(),
            pending
                .iter()
                .take(10)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(())
}

fn validate_owner(crate_name: &str, source: &str) -> Result<(), String> {
    if !matches!(crate_name, "hl-client" | "hl-daemon" | "hl-images") {
        return Err(format!("unknown historical crate {crate_name:?}"));
    }
    if !source.starts_with(&format!("{crate_name}/")) {
        return Err(format!(
            "source {source:?} is not owned by historical crate {crate_name:?}"
        ));
    }
    Ok(())
}

pub(crate) mod tests {
    pub(crate) fn rejects_patch_artifacts_and_mismatched_sources() {
        assert!(super::validate_owner("+hl-daemon", "hl-daemon/src/events.rs").is_err());
        assert!(super::validate_owner("hl-images", "hl-daemon/src/events.rs").is_err());
        assert!(super::validate_owner("hl-daemon", "hl-daemon/src/events.rs").is_ok());
    }
}

fn audit_images() -> Result<(), String> {
    let mut manifest = BTreeSet::new();
    for (index, line) in IMAGE_MANIFEST.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if !manifest.insert(line.to_owned()) {
            return Err(format!(
                "duplicate image reference {line:?} at images.manifest:{}",
                index + 1
            ));
        }
    }
    if manifest.len() != EXPECTED_IMAGES {
        return Err(format!(
            "image manifest has {} references; expected {EXPECTED_IMAGES}",
            manifest.len()
        ));
    }

    let mut contracts = BTreeSet::new();
    for (index, line) in SCENARIOS.lines().enumerate() {
        let value: serde_json::Value = serde_json::from_str(line)
            .map_err(|error| format!("scenario.contracts:{}: {error}", index + 1))?;
        let image = value
            .get("image")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("scenario.contracts:{} has no string image", index + 1))?;
        contracts.insert(image.to_owned());
    }
    let missing = contracts.difference(&manifest).cloned().collect::<Vec<_>>();
    let unregistered = manifest.difference(&contracts).cloned().collect::<Vec<_>>();
    if !missing.is_empty() || !unregistered.is_empty() {
        return Err(format!(
            "image provenance drift: missing={missing:?}, unregistered={unregistered:?}"
        ));
    }
    println!(
        "OCI fixture manifest: {} exact references; zero scenario references missing",
        manifest.len()
    );
    Ok(())
}
