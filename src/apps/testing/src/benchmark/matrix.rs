use super::{
    Isa, LIMIT, Provider, Run, adapter, alternating, file_identity, host_affinity, host_snapshot, identity_field,
    parse_assignment, parse_duration, report,
};
use clap::Args;
use sha2::Digest as _;
use std::{path::PathBuf, time::Duration};

#[derive(Args, Debug)]
pub(crate) struct Matrix {
    #[arg(long = "arch", value_enum)]
    isa: Isa,
    #[arg(long)]
    binary: PathBuf,
    #[arg(long)]
    rootfs: Option<PathBuf>,
    #[arg(long)]
    c_engine: PathBuf,
    #[arg(long)]
    rust_engine: PathBuf,
    #[arg(long = "out", default_value = "target/testing/benchmark-matrix")]
    output: PathBuf,
    #[arg(long, default_value_t = 3)]
    repeats: usize,
    /// Continue only rows recorded for this exact runner, rootfs, guest, and engine content.
    #[arg(long)]
    resume: bool,
    /// Optional bounded guest/provider clone/thread evidence captured beside each row.
    #[arg(long)]
    clone_thread_evidence: Option<PathBuf>,
    #[arg(long, default_value = "120", value_parser = parse_duration)]
    timeout: Duration,
    #[arg(last = true, num_args = 0.., allow_hyphen_values = true)]
    guest: Vec<String>,
    #[arg(long = "env", value_parser = parse_assignment)]
    environment: Vec<(String, String)>,
    #[arg(long = "engine-option", value_parser = parse_assignment)]
    engine_options: Vec<(String, String)>,
}

impl Matrix {
    pub(super) fn validate(mut self) -> Result<Self, String> {
        let host_matches = matches!(
            (std::env::consts::ARCH, self.isa),
            ("aarch64", Isa::Aarch64) | ("x86_64", Isa::X86)
        );
        if !host_matches {
            return Err("matrix requires a host-native baseline for the selected architecture".into());
        }
        if self.output.as_os_str().is_empty() {
            return Err("matrix output directory cannot be empty".into());
        }
        if self.repeats == 0 || self.repeats > LIMIT {
            return Err(format!("repeats must be between 1 and {LIMIT}"));
        }
        #[cfg(target_os = "linux")]
        {
            let affinity = host_affinity();
            if affinity == "unknown" {
                return Err("matrix cannot establish inherited CPU affinity".into());
            }
            if affinity.contains(',') || affinity.contains('-') {
                return Err("matrix requires one inherited CPU; run it under taskset -c <cpu>".into());
            }
        }
        self.require_native_options()?;
        Ok(self)
    }

    fn require_native_options(&mut self) -> Result<(), String> {
        for name in ["HL_NATIVE_EXECUTION", "HL_NATIVE_DIAGNOSTICS"] {
            let values = self
                .engine_options
                .iter()
                .filter(|(candidate, _)| candidate == name)
                .map(|(_, value)| value.as_str())
                .collect::<Vec<_>>();
            match values.as_slice() {
                [] => self.engine_options.push((name.into(), "1".into())),
                ["1"] => {}
                [_] => return Err(format!("matrix requires {name}=1")),
                _ => return Err(format!("matrix accepts {name} only once")),
            }
        }
        Ok(())
    }

    pub(super) fn execute(self, process: &adapter::Process) -> Result<(), String> {
        self.execute_with(|run| run.validate()?.execute(process))
    }

    fn execute_with<F>(self, mut execute: F) -> Result<(), String>
    where
        F: FnMut(Run) -> Result<(), String>,
    {
        std::fs::create_dir_all(&self.output).map_err(|error| format!("matrix output directory: {error}"))?;
        eprintln!("benchmark-host {}", host_snapshot());
        let identity = self.identity()?;
        let mut paths = Vec::new();
        for cycle in 1..=self.repeats {
            for provider in [Provider::Native, Provider::C, Provider::Rust] {
                paths.push(self.output_path(cycle, provider));
            }
        }
        let journal = self.output.join("alternating.tsv");
        alternating::run(
            u32::try_from(self.repeats).map_err(|_| "matrix repeat overflow")?,
            &journal,
            &identity,
            self.resume,
            |step| {
                let run = self.run(step);
                let output = run.output.clone().expect("scheduled matrix run has output");
                execute(run)?;
                let mut evidence = std::fs::read(&output).map_err(|error| format!("matrix row evidence: {error}"))?;
                if let Some(path) = &self.clone_thread_evidence {
                    let hook = std::fs::read(path).map_err(|error| format!("clone/thread evidence: {error}"))?;
                    evidence.extend_from_slice(&hook);
                }
                String::from_utf8(evidence).map_err(|_| "matrix evidence is not UTF-8".into())
            },
        )?;
        report::Report::new("native", paths).write()
    }

    fn run(&self, step: alternating::Step) -> Run {
        let provider = match step.provider {
            alternating::Provider::Native => Provider::Native,
            alternating::Provider::C => Provider::C,
            alternating::Provider::Rust => Provider::Rust,
        };
        let engine = match provider {
            Provider::C => Some(self.c_engine.clone()),
            Provider::Rust => Some(self.rust_engine.clone()),
            _ => None,
        };
        let mut engine_options = if provider == Provider::Rust {
            self.engine_options.clone()
        } else {
            Vec::new()
        };
        engine_options.retain(|(name, _)| name != "HL_NATIVE_DIAGNOSTICS");
        if step.mode == alternating::Mode::DiagnosticsProof {
            engine_options.push(("HL_NATIVE_DIAGNOSTICS".into(), "1".into()));
        }
        Run {
            provider,
            isa: self.isa,
            binary: self.binary.clone(),
            rootfs: self.rootfs.clone(),
            engine,
            output: Some(if step.mode == alternating::Mode::DiagnosticsProof {
                self.output.join("rust-engine-diagnostics-proof.csv")
            } else {
                self.output_path(step.cycle as usize, provider)
            }),
            repeats: 1,
            timeout: self.timeout,
            guest: self.guest.clone(),
            environment: self.environment.clone(),
            engine_options,
            diagnostics_proven: true,
        }
    }

    fn output_path(&self, cycle: usize, provider: Provider) -> PathBuf {
        self.output.join(format!(
            "cycle-{cycle:03}-{}-{}.csv",
            provider.name(),
            self.isa.public()
        ))
    }

    fn identity(&self) -> Result<String, String> {
        let mut digest = sha2::Sha256::new();
        for path in [&self.c_engine, &self.rust_engine] {
            identity_field(&mut digest, file_identity(path)?.as_bytes());
        }
        let guest = self.rootfs.as_ref().map_or_else(
            || self.binary.clone(),
            |root| root.join(self.binary.strip_prefix("/").unwrap_or(&self.binary)),
        );
        identity_field(&mut digest, file_identity(&guest)?.as_bytes());
        identity_field(
            &mut digest,
            file_identity(&std::env::current_exe().map_err(|error| format!("runner executable: {error}"))?)?.as_bytes(),
        );
        for value in [
            self.isa.public(),
            &self.repeats.to_string(),
            &self.timeout.as_nanos().to_string(),
        ] {
            identity_field(&mut digest, value.as_bytes());
        }
        for (name, value) in self.environment.iter().chain(&self.engine_options) {
            identity_field(&mut digest, name.as_bytes());
            identity_field(&mut digest, value.as_bytes());
        }
        for value in &self.guest {
            identity_field(&mut digest, value.as_bytes());
        }
        Ok(super::hex_digest(digest.finalize()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_row_is_always_native_verified() {
        let isa = if std::env::consts::ARCH == "x86_64" {
            Isa::X86
        } else {
            Isa::Aarch64
        };
        let mut matrix = Matrix {
            isa,
            binary: "/guest".into(),
            rootfs: None,
            c_engine: "/c-engine".into(),
            rust_engine: "/rust-engine".into(),
            output: "/results".into(),
            repeats: 1,
            resume: false,
            clone_thread_evidence: None,
            timeout: Duration::from_secs(1),
            guest: Vec::new(),
            environment: Vec::new(),
            engine_options: Vec::new(),
        };
        matrix.require_native_options().unwrap();
        let plan = alternating::plan(1).unwrap();
        let proof = matrix.run(plan[0]).validate().unwrap();
        assert_eq!(proof.execution_mode(), "native-verified");
        assert!(
            proof
                .engine_options
                .contains(&("HL_NATIVE_EXECUTION".into(), "1".into()))
        );
        assert!(
            proof
                .engine_options
                .contains(&("HL_NATIVE_DIAGNOSTICS".into(), "1".into()))
        );
        let timed = plan
            .into_iter()
            .find(|step| step.provider == alternating::Provider::Rust && step.mode == alternating::Mode::Timing)
            .map(|step| matrix.run(step).validate().unwrap())
            .unwrap();
        assert!(
            timed
                .engine_options
                .contains(&("HL_NATIVE_EXECUTION".into(), "1".into()))
        );
        assert!(
            !timed
                .engine_options
                .iter()
                .any(|(name, _)| name == "HL_NATIVE_DIAGNOSTICS")
        );
    }

    #[test]
    fn fake_executor_observes_proof_then_latin_timing_rows() {
        let directory = tempfile::tempdir().unwrap();
        let binary = directory.path().join("guest");
        let c_engine = directory.path().join("c");
        let rust_engine = directory.path().join("rust");
        for path in [&binary, &c_engine, &rust_engine] {
            std::fs::write(path, b"content").unwrap();
        }
        let matrix = Matrix {
            isa: if std::env::consts::ARCH == "x86_64" {
                Isa::X86
            } else {
                Isa::Aarch64
            },
            binary,
            rootfs: None,
            c_engine,
            rust_engine,
            output: directory.path().join("out"),
            repeats: 3,
            resume: false,
            clone_thread_evidence: None,
            timeout: Duration::from_secs(1),
            guest: Vec::new(),
            environment: Vec::new(),
            engine_options: vec![
                ("HL_NATIVE_EXECUTION".into(), "1".into()),
                ("HL_NATIVE_DIAGNOSTICS".into(), "1".into()),
            ],
        };
        let mut seen = Vec::new();
        matrix.execute_with(|run| {
            seen.push((run.provider, run.engine_options.iter().any(|(name, _)| name == "HL_NATIVE_DIAGNOSTICS")));
            std::fs::write(run.output.unwrap(), "env,arch,phase,us,ok,us_min,us_max,repeats,wall_us,execution,guest_sha256,engine_sha256,runner_sha256,options_sha256,cpu_affinity\nnative,amd64,p,1,1,1,1,1,1,host-native,a,b,c,d,0\n").unwrap();
            Ok(())
        }).unwrap();
        assert_eq!(
            seen,
            [
                (Provider::Rust, true),
                (Provider::Native, false),
                (Provider::C, false),
                (Provider::Rust, false),
                (Provider::C, false),
                (Provider::Rust, false),
                (Provider::Native, false),
                (Provider::Rust, false),
                (Provider::Native, false),
                (Provider::C, false)
            ]
        );
    }
}
