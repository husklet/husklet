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
    /// Optional typed exec wrapper. The retained engine remains `--c-engine`.
    #[arg(long)]
    c_runner: Option<PathBuf>,
    /// Auditable source-tree revision asserted by the build producer. Binary
    /// identity is established independently by SHA-256 and ELF BuildID.
    #[arg(long)]
    c_engine_tree: String,
    /// Expected ELF BuildID, verified directly against `--c-engine`.
    #[arg(long)]
    c_engine_build_id: String,
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
        if self.c_engine_tree.trim().is_empty() || self.c_engine_build_id.trim().is_empty() {
            return Err("matrix requires nonempty retained C tree and build identifiers".into());
        }
        let build_id = elf_build_id(&self.c_engine)?;
        if !self.c_engine_build_id.eq_ignore_ascii_case(&build_id) {
            return Err(format!(
                "retained C BuildID mismatch: expected {}, binary has {build_id}",
                self.c_engine_build_id
            ));
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
        let engine_sha = file_identity(&self.c_engine)?;
        let runner_sha = self
            .c_runner
            .as_ref()
            .map(|path| file_identity(path))
            .transpose()?;
        std::fs::write(
            self.output.join("c-engine-provenance.tsv"),
            format!(
                "tree\tbuild_id\tengine_sha256\trunner_sha256\n{}\t{}\t{}\t{}\n",
                self.c_engine_tree,
                self.c_engine_build_id,
                engine_sha,
                runner_sha.as_deref().unwrap_or("direct"),
            ),
        )
        .map_err(|error| format!("C engine provenance: {error}"))?;
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
            c_runner: if provider == Provider::C {
                self.c_runner.clone()
            } else {
                None
            },
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
        if let Some(runner) = &self.c_runner {
            identity_field(&mut digest, file_identity(runner)?.as_bytes());
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
            &self.c_engine_tree,
            &self.c_engine_build_id,
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

fn elf_build_id(path: &std::path::Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("read retained C ELF: {error}"))?;
    if bytes.get(..4) != Some(b"\x7fELF") || bytes.get(4) != Some(&2) {
        return Err("retained C engine must be a 64-bit ELF with a GNU BuildID".into());
    }
    let little = match bytes.get(5) {
        Some(1) => true,
        Some(2) => false,
        _ => return Err("retained C ELF has unsupported byte order".into()),
    };
    let u32_at = |offset: usize| -> Result<u32, String> {
        let raw: [u8; 4] = bytes
            .get(offset..offset + 4)
            .ok_or("truncated retained C ELF")?
            .try_into()
            .map_err(|_| "truncated retained C ELF")?;
        Ok(if little { u32::from_le_bytes(raw) } else { u32::from_be_bytes(raw) })
    };
    let u64_at = |offset: usize| -> Result<u64, String> {
        let raw: [u8; 8] = bytes
            .get(offset..offset + 8)
            .ok_or("truncated retained C ELF")?
            .try_into()
            .map_err(|_| "truncated retained C ELF")?;
        Ok(if little { u64::from_le_bytes(raw) } else { u64::from_be_bytes(raw) })
    };
    let section_offset = usize::try_from(u64_at(40)?).map_err(|_| "retained C ELF section offset overflow")?;
    let section_size = usize::from(if little {
        u16::from_le_bytes(bytes.get(58..60).ok_or("truncated retained C ELF")?.try_into().map_err(|_| "truncated retained C ELF")?)
    } else {
        u16::from_be_bytes(bytes.get(58..60).ok_or("truncated retained C ELF")?.try_into().map_err(|_| "truncated retained C ELF")?)
    });
    let section_count = usize::from(if little {
        u16::from_le_bytes(bytes.get(60..62).ok_or("truncated retained C ELF")?.try_into().map_err(|_| "truncated retained C ELF")?)
    } else {
        u16::from_be_bytes(bytes.get(60..62).ok_or("truncated retained C ELF")?.try_into().map_err(|_| "truncated retained C ELF")?)
    });
    for index in 0..section_count {
        let header = section_offset.checked_add(index.checked_mul(section_size).ok_or("retained C ELF section overflow")?).ok_or("retained C ELF section overflow")?;
        if u32_at(header + 4)? != 7 { continue; }
        let start = usize::try_from(u64_at(header + 24)?).map_err(|_| "retained C ELF note offset overflow")?;
        let size = usize::try_from(u64_at(header + 32)?).map_err(|_| "retained C ELF note size overflow")?;
        let end = start.checked_add(size).ok_or("retained C ELF note overflow")?;
        let mut cursor = start;
        while cursor + 12 <= end {
            let namesz = usize::try_from(u32_at(cursor)?).map_err(|_| "retained C ELF note overflow")?;
            let descsz = usize::try_from(u32_at(cursor + 4)?).map_err(|_| "retained C ELF note overflow")?;
            let kind = u32_at(cursor + 8)?;
            let name = cursor.checked_add(12).ok_or("retained C ELF note overflow")?;
            let padded_name = namesz.checked_add(3).ok_or("retained C ELF note overflow")? & !3;
            let padded_desc = descsz.checked_add(3).ok_or("retained C ELF note overflow")? & !3;
            let desc = name.checked_add(padded_name).ok_or("retained C ELF note overflow")?;
            let next = desc.checked_add(padded_desc).ok_or("retained C ELF note overflow")?;
            if next > end { return Err("truncated retained C ELF note".into()); }
            let name_end = name.checked_add(namesz).ok_or("retained C ELF note overflow")?;
            if kind == 3 && bytes.get(name..name_end) == Some(b"GNU\0") {
                return Ok(bytes[desc..desc + descsz].iter().map(|byte| format!("{byte:02x}")).collect());
            }
            cursor = next;
        }
    }
    Err("retained C ELF has no GNU BuildID".into())
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
            c_runner: None,
            c_engine_tree: "tree".into(),
            c_engine_build_id: "build".into(),
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
        assert!(!timed.native_diagnostics_requested());
        assert!(proof.native_diagnostics_requested());
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
            c_runner: None,
            c_engine_tree: "tree".into(),
            c_engine_build_id: "build".into(),
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

    #[test]
    fn extracts_authoritative_build_id_from_current_elf() {
        let executable = std::env::current_exe().unwrap();
        assert!(!elf_build_id(&executable).unwrap().is_empty());
    }
}
