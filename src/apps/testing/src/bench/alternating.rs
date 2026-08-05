//! Balanced provider ordering with durable, content-bound resume admission.

use crate::suite::Error;
use std::{collections::BTreeSet, fs::OpenOptions, future::Future, io::Write, path::Path};

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

pub(super) fn plan(cycles: u32) -> Result<Vec<Step>, Error> {
    if !(1..=MAX_CYCLES).contains(&cycles) {
        return Err("alternating benchmark cycles must be between 1 and 128".into());
    }
    let providers = [Provider::Native, Provider::C, Provider::Rust];
    let mut steps = vec![Step { cycle: 0, provider: Provider::Rust, mode: Mode::DiagnosticsProof }];
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
pub(super) async fn run<F, Fut>(
    cycles: u32,
    journal: &Path,
    identity: &str,
    resume: bool,
    mut execute: F,
) -> Result<(), Error>
where
    F: FnMut(Step) -> Fut,
    Fut: Future<Output = Result<String, Error>>,
{
    if identity.contains(['\t', '\n']) {
        return Err("unsafe alternating benchmark identity".into());
    }
    let steps = plan(cycles)?;
    let expected = steps.iter().copied().collect::<BTreeSet<_>>();
    let mut completed = if resume && journal.exists() {
        load(journal, identity, &expected)?
    } else {
        if let Some(parent) = journal.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(journal, format!("# alternating-v1\t{identity}\ncycle\tprovider\tmode\tevidence\n"))?;
        BTreeSet::new()
    };
    let mut output = OpenOptions::new().append(true).open(journal)?;
    for step in steps {
        if completed.contains(&step) {
            continue;
        }
        let evidence = execute(step).await?;
        if evidence.len() > MAX_EVIDENCE {
            return Err("alternating benchmark evidence exceeds its byte bound".into());
        }
        writeln!(output, "{}\t{}\t{}\t{}", step.cycle, provider(step.provider), mode(step.mode), hex(evidence.as_bytes()))?;
        output.sync_data()?;
        completed.insert(step);
    }
    Ok(())
}

fn load(path: &Path, identity: &str, expected: &BTreeSet<Step>) -> Result<BTreeSet<Step>, Error> {
    let text = std::fs::read_to_string(path)?;
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
            cycle: fields[0].parse()?,
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
    match value { Provider::Native => "native", Provider::C => "c-engine", Provider::Rust => "rust-engine" }
}
const fn mode(value: Mode) -> &'static str {
    match value { Mode::DiagnosticsProof => "diagnostics-proof", Mode::Timing => "timing" }
}
fn parse_provider(value: &str) -> Result<Provider, Error> {
    match value { "native" => Ok(Provider::Native), "c-engine" => Ok(Provider::C), "rust-engine" => Ok(Provider::Rust), _ => Err("invalid provider".into()) }
}
fn parse_mode(value: &str) -> Result<Mode, Error> {
    match value { "diagnostics-proof" => Ok(Mode::DiagnosticsProof), "timing" => Ok(Mode::Timing), _ => Err("invalid mode".into()) }
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }

#[cfg(test)]
mod tests {
    use super::{Mode, Provider, plan, run};
    use std::sync::{Arc, Mutex};

    #[test]
    fn proof_precedes_balanced_alternation() {
        let steps = plan(3).unwrap();
        assert_eq!((steps[0].provider, steps[0].mode), (Provider::Rust, Mode::DiagnosticsProof));
        assert_eq!(steps[1..].iter().map(|step| step.provider).collect::<Vec<_>>(), [
            Provider::Native, Provider::C, Provider::Rust,
            Provider::C, Provider::Rust, Provider::Native,
            Provider::Rust, Provider::Native, Provider::C,
        ]);
    }

    #[tokio::test]
    async fn exact_identity_resume_skips_durable_steps() {
        let directory = tempfile::tempdir().unwrap();
        let journal = directory.path().join("rows.tsv");
        let calls = Arc::new(Mutex::new(0));
        let first = Arc::clone(&calls);
        assert!(run(1, &journal, "tree-rootfs-artifacts", false, move |_| {
            let first = Arc::clone(&first);
            async move { let mut n = first.lock().unwrap(); *n += 1; if *n == 3 { Err("stop".into()) } else { Ok("bounded".into()) } }
        }).await.is_err());
        let resumed = Arc::clone(&calls);
        run(1, &journal, "tree-rootfs-artifacts", true, move |_| {
            let resumed = Arc::clone(&resumed);
            async move { *resumed.lock().unwrap() += 1; Ok("bounded".into()) }
        }).await.unwrap();
        assert_eq!(*calls.lock().unwrap(), 5);
        assert!(run(1, &journal, "changed", true, |_| async { Ok("bounded".into()) }).await.is_err());
    }
}
