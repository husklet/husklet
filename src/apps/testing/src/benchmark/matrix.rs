use super::{
    Isa, LIMIT, Provider, Run, adapter, host_affinity, host_snapshot, parse_assignment, parse_duration, report,
};
use clap::Args;
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
        std::fs::create_dir_all(&self.output).map_err(|error| format!("matrix output directory: {error}"))?;
        eprintln!("benchmark-host {}", host_snapshot());
        let runs = self.runs();
        let paths = runs
            .iter()
            .map(|run| run.output.clone().expect("matrix run has an output"))
            .collect::<Vec<_>>();
        for run in runs {
            run.validate()?.execute(process)?;
        }
        report::Report::new("native", paths).write()
    }

    fn runs(&self) -> Vec<Run> {
        [
            (Provider::Native, None, Vec::new()),
            (Provider::C, Some(self.c_engine.clone()), Vec::new()),
            (
                Provider::Rust,
                Some(self.rust_engine.clone()),
                self.engine_options.clone(),
            ),
        ]
        .into_iter()
        .map(|(provider, engine, engine_options)| Run {
            provider,
            isa: self.isa,
            binary: self.binary.clone(),
            rootfs: self.rootfs.clone(),
            engine,
            output: Some(
                self.output
                    .join(format!("{}-{}.csv", provider.name(), self.isa.public())),
            ),
            repeats: self.repeats,
            timeout: self.timeout,
            guest: self.guest.clone(),
            environment: self.environment.clone(),
            engine_options,
        })
        .collect()
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
            timeout: Duration::from_secs(1),
            guest: Vec::new(),
            environment: Vec::new(),
            engine_options: Vec::new(),
        };
        matrix.require_native_options().unwrap();
        let rust = matrix
            .runs()
            .into_iter()
            .find(|run| run.provider == Provider::Rust)
            .unwrap()
            .validate()
            .unwrap();
        assert_eq!(rust.execution_mode(), "native-verified");
        for name in ["HL_NATIVE_EXECUTION", "HL_NATIVE_DIAGNOSTICS"] {
            assert!(
                rust.engine_options
                    .iter()
                    .any(|option| option == &(name.into(), "1".into()))
            );
        }
    }
}
