//! Balanced provider ordering with durable, content-bound resume admission.

use std::{collections::BTreeSet, fs::OpenOptions, io::Write, path::Path};

const MAX_CYCLES: u32 = 128;
const MAX_EVIDENCE: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum Provider {
    Native,
    C,
    Rust,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum Mode {
    DiagnosticsProof,
    Timing,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct Step {
    pub cycle: u32,
    pub provider: Provider,
    pub mode: Mode,
}

#[cfg(test)]
pub(super) fn plan(cycles: u32) -> Result<Vec<Step>, String> {
    plan_over(cycles, &[Provider::Native, Provider::C, Provider::Rust])
}

/// Balances the given providers so ordering cannot favour one of them.
pub(super) fn plan_over(cycles: u32, providers: &[Provider]) -> Result<Vec<Step>, String> {
    if !(1..=MAX_CYCLES).contains(&cycles) {
        return Err("alternating benchmark cycles must be between 1 and 128".into());
    }
    if providers.is_empty() {
        return Err("alternating benchmark requires at least one provider".into());
    }
    let mut steps = vec![Step {
        cycle: 0,
        provider: Provider::Rust,
        mode: Mode::DiagnosticsProof,
    }];
    for cycle in 0..cycles {
        let offset = cycle as usize % providers.len();
        for ordinal in 0..providers.len() {
            steps.push(Step {
                cycle: cycle + 1,
                provider: providers[(offset + ordinal) % providers.len()],
                mode: Mode::Timing,
            });
        }
    }
    Ok(steps)
}

/// Runs one diagnostics-on Rust proof, then diagnostics-off timing steps.
pub(super) fn run<F>(
    cycles: u32,
    providers: &[Provider],
    journal: &Path,
    identity: &str,
    resume: bool,
    mut execute: F,
) -> Result<(), String>
where
    F: FnMut(Step) -> Result<String, String>,
{
    if identity.contains(['\t', '\n']) {
        return Err("unsafe alternating benchmark identity".into());
    }
    let steps = plan_over(cycles, providers)?;
    let expected = steps.iter().copied().collect::<BTreeSet<_>>();
    let mut completed = if resume && journal.exists() {
        load(journal, identity, &expected)?
    } else {
        if let Some(parent) = journal.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        std::fs::write(
            journal,
            format!("# alternating-v1\t{identity}\ncycle\tprovider\tmode\tevidence\n"),
        )
        .map_err(|error| error.to_string())?;
        BTreeSet::new()
    };
    let mut output = OpenOptions::new()
        .append(true)
        .open(journal)
        .map_err(|error| error.to_string())?;
    for step in steps {
        if completed.contains(&step) {
            continue;
        }
        let evidence = execute(step)?;
        if evidence.len() > MAX_EVIDENCE {
            return Err("alternating benchmark evidence exceeds its byte bound".into());
        }
        writeln!(
            output,
            "{}\t{}\t{}\t{}",
            step.cycle,
            provider(step.provider),
            mode(step.mode),
            hex(evidence.as_bytes())
        )
        .map_err(|error| error.to_string())?;
        output.sync_data().map_err(|error| error.to_string())?;
        completed.insert(step);
    }
    Ok(())
}

fn load(path: &Path, identity: &str, expected: &BTreeSet<Step>) -> Result<BTreeSet<Step>, String> {
    let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let mut lines = text.lines();
    if lines.next() != Some(format!("# alternating-v1\t{identity}").as_str())
        || lines.next() != Some("cycle\tprovider\tmode\tevidence")
    {
        return Err("alternating benchmark resume identity or schema changed".into());
    }
    let mut completed = BTreeSet::new();
    for row in lines {
        let fields = row.split('\t').collect::<Vec<_>>();
        if fields.len() != 4 || fields[3].len() > MAX_EVIDENCE * 2 {
            return Err("invalid alternating benchmark row".into());
        }
        let step = Step {
            cycle: fields[0].parse().map_err(|_| "invalid benchmark cycle")?,
            provider: parse_provider(fields[1])?,
            mode: parse_mode(fields[2])?,
        };
        if !expected.contains(&step) || !completed.insert(step) {
            return Err("stale or duplicate alternating benchmark row".into());
        }
    }
    Ok(completed)
}

const fn provider(value: Provider) -> &'static str {
    match value {
        Provider::Native => "native",
        Provider::C => "c-engine",
        Provider::Rust => "rust-engine",
    }
}
const fn mode(value: Mode) -> &'static str {
    match value {
        Mode::DiagnosticsProof => "diagnostics-proof",
        Mode::Timing => "timing",
    }
}
fn parse_provider(value: &str) -> Result<Provider, String> {
    match value {
        "native" => Ok(Provider::Native),
        "c-engine" => Ok(Provider::C),
        "rust-engine" => Ok(Provider::Rust),
        _ => Err("invalid provider".into()),
    }
}
fn parse_mode(value: &str) -> Result<Mode, String> {
    match value {
        "diagnostics-proof" => Ok(Mode::DiagnosticsProof),
        "timing" => Ok(Mode::Timing),
        _ => Err("invalid mode".into()),
    }
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::{Mode, Provider, plan, plan_over, run};
    use std::sync::{Arc, Mutex};

    const ALL: [Provider; 3] = [Provider::Native, Provider::C, Provider::Rust];

    #[test]
    fn engine_only_plans_drop_the_native_baseline() {
        let steps = plan_over(2, &[Provider::C, Provider::Rust]).unwrap();
        assert!(steps[1..].iter().all(|step| step.provider != Provider::Native));
        assert_eq!(steps.len(), 5);
        assert!(plan_over(1, &[]).is_err());
    }

    #[test]
    fn proof_precedes_balanced_alternation() {
        let steps = plan(3).unwrap();
        assert_eq!(
            (steps[0].provider, steps[0].mode),
            (Provider::Rust, Mode::DiagnosticsProof)
        );
        assert_eq!(
            steps[1..].iter().map(|step| step.provider).collect::<Vec<_>>(),
            [
                Provider::Native,
                Provider::C,
                Provider::Rust,
                Provider::C,
                Provider::Rust,
                Provider::Native,
                Provider::Rust,
                Provider::Native,
                Provider::C,
            ]
        );
    }

    #[test]
    fn exact_identity_resume_skips_durable_steps() {
        let directory = tempfile::tempdir().unwrap();
        let journal = directory.path().join("rows.tsv");
        let calls = Arc::new(Mutex::new(0));
        let first = Arc::clone(&calls);
        assert!(
            run(1, &ALL, &journal, "tree-rootfs-artifacts", false, move |_| {
                let mut n = first.lock().unwrap();
                *n += 1;
                if *n == 3 {
                    Err("stop".into())
                } else {
                    Ok("bounded".into())
                }
            })
            .is_err()
        );
        let resumed = Arc::clone(&calls);
        run(1, &ALL, &journal, "tree-rootfs-artifacts", true, move |_| {
            *resumed.lock().unwrap() += 1;
            Ok("bounded".into())
        })
        .unwrap();
        assert_eq!(*calls.lock().unwrap(), 5);
        assert!(run(1, &ALL, &journal, "changed", true, |_| Ok("bounded".into())).is_err());
    }
}
