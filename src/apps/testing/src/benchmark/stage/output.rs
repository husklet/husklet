use super::*;

pub(super) fn raw_sha256(path: &Path) -> Result<String, Error> {
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    let hash = digest.finalize();
    let mut hex = String::with_capacity(64);
    for byte in hash {
        write!(hex, "{byte:02x}")?;
    }
    Ok(hex)
}

pub(super) fn native_library(build: &Path) -> Result<PathBuf, Error> {
    let directory = build.join("release/build");
    let libraries = fs::read_dir(&directory)?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|entry| entry.path().join("out/libhl_native_engine.dylib"))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    let [library] = libraries.as_slice() else {
        return Err("native macOS build did not produce exactly one native engine library".into());
    };
    Ok(library.clone())
}

fn checked(program: &Path, arguments: &[String]) -> Result<Vec<u8>, Error> {
    let captured = HostProcess::bounded_capture(program, arguments, TIMEOUT)?;
    if captured.outcome != Outcome::Exited(Some(0)) {
        return Err(format!(
            "stage command failed with {:?}: {}",
            captured.outcome,
            String::from_utf8_lossy(&captured.stderr)
        )
        .into());
    }
    if !captured.stderr.is_empty() {
        return Err(format!(
            "stage command wrote stderr: {}",
            String::from_utf8_lossy(&captured.stderr)
        )
        .into());
    }
    Ok(captured.stdout)
}

pub(super) fn mac(arguments: &[String]) -> Result<Vec<u8>, Error> {
    checked(Path::new(MAC), arguments)
}

pub(crate) fn mac_preparation_compile(arguments: &[String]) -> Result<Vec<u8>, Error> {
    let captured = HostProcess::bounded_capture(Path::new(MAC), arguments, PREPARATION_COMPILE_TIMEOUT)?;
    if captured.outcome != Outcome::Exited(Some(0)) {
        return Err(format!(
            "stage preparation compile failed with {:?}: {}",
            captured.outcome,
            String::from_utf8_lossy(&captured.stderr)
        )
        .into());
    }
    Ok(captured.stdout)
}

#[cfg(test)]
mod timeout_tests {
    use super::{PREPARATION_COMPILE_TIMEOUT, PYTHON_TIMEOUT, TIMEOUT};
    use std::time::Duration;

    #[test]
    fn long_timeout_is_scoped_to_preparation_compilation() {
        assert_eq!(TIMEOUT, Duration::from_secs(30));
        assert_eq!(PYTHON_TIMEOUT, Duration::from_secs(90));
        assert_eq!(PREPARATION_COMPILE_TIMEOUT, Duration::from_secs(180));
    }
}

pub(super) fn husklet_rootfs_guest(
    command: &Path,
    rootfs: &Path,
    guest: &str,
    guest_arguments: &[&str],
) -> Result<Vec<u8>, Error> {
    let mut arguments = vec![
        mac_path(command),
        "--rootfs".into(),
        rootfs.display().to_string(),
        guest.into(),
    ];
    arguments.extend(guest_arguments.iter().map(|argument| (*argument).to_owned()));
    let captured = HostProcess::bounded_capture(Path::new(MAC), &arguments, PYTHON_TIMEOUT)?;
    let displaced = b"hl-test-displaced-et-exec: displaced\n";
    if captured.outcome != Outcome::Exited(Some(0)) || (!captured.stderr.is_empty() && captured.stderr != displaced) {
        return Err(format!(
            "native-arm64 Husklet x86 rootfs guest failed with {:?}: {}",
            captured.outcome,
            String::from_utf8_lossy(&captured.stderr)
        )
        .into());
    }
    Ok(captured.stdout)
}

pub(super) fn capture_rootfs_guest(
    command: &Path,
    rootfs: &Path,
    guest: &str,
    guest_arguments: &[&str],
) -> Result<ProcessCapture, Error> {
    let mut arguments = vec![
        mac_path(command),
        "--rootfs".into(),
        rootfs.display().to_string(),
        guest.into(),
    ];
    arguments.extend(guest_arguments.iter().map(|argument| (*argument).to_owned()));
    Ok(HostProcess::bounded_capture(
        Path::new(MAC),
        &arguments,
        PYTHON_TIMEOUT,
    )?)
}

pub(super) struct PythonHusklet {
    pub(super) interpreter: PathBuf,
    pub(super) rootfs: PathBuf,
}

impl PythonHusklet {
    pub(super) fn stage(output: &Path, rootfs: &Path, command: &Path, factor: &str) -> Result<Self, Error> {
        let interpreter = rootfs.join("usr/local/bin/python3.12");
        let arguments = [
            mac_path(command),
            "--rootfs".into(),
            rootfs.display().to_string(),
            "usr/local/bin/python3.12".into(),
            "-B".into(),
            "-c".into(),
            python::PLAIN_PROGRAM.into(),
            factor.into(),
        ];
        let captured = HostProcess::bounded_capture(Path::new(MAC), &arguments, PYTHON_TIMEOUT)?;
        if captured.outcome != Outcome::Exited(Some(0)) || !captured.stderr.is_empty() {
            return Err(format!(
                "native-arm64 Husklet x86 Python failed with {:?}: {}",
                captured.outcome,
                String::from_utf8_lossy(&captured.stderr)
            )
            .into());
        }
        let native_frame = fs::read(output.join("python-plain-exact-output.frame"))?;
        let husklet_frame = python::profile_frame("plain", &captured.stdout)?;
        require_parity("python/plain Husklet", &native_frame, &husklet_frame)?;
        fs::write(output.join("python-plain-husklet.out"), captured.stdout)?;
        fs::write(output.join("python-plain-husklet-exact-output.frame"), husklet_frame)?;
        Ok(Self {
            interpreter,
            rootfs: rootfs.to_path_buf(),
        })
    }
}

pub(super) fn mac_path(path: &Path) -> String {
    format!("/mnt/mac{}", path.display())
}

pub(super) fn frame(output: &[u8]) -> Result<Vec<u8>, Error> {
    let text = std::str::from_utf8(output)?;
    if !text.ends_with('\n') || text.contains('\r') {
        return Err("staged workload output is not LF framed".into());
    }
    let mut framed = Vec::new();
    let mut metadata = 0_usize;
    for line in text.lines() {
        if line.starts_with("META ") {
            metadata += 1;
            framed.push(line.to_owned());
            continue;
        }
        let rest = line
            .strip_prefix("PHASE ")
            .ok_or("staged workload emitted an unaccounted line")?;
        let fields = rest.split_ascii_whitespace().collect::<Vec<_>>();
        let [name, micros, ok] = fields.as_slice() else {
            return Err("staged PHASE must have exactly name, us, and ok fields".into());
        };
        if name.is_empty()
            || micros
                .strip_prefix("us=")
                .is_none_or(|value| value.parse::<u64>().is_err())
            || ok.strip_prefix("ok=").is_none_or(str::is_empty)
        {
            return Err("staged PHASE fields are invalid".into());
        }
        framed.push(format!("PHASE {name} us=<time> {ok}"));
    }
    if metadata != 1 {
        return Err("staged workload must emit exactly one META line".into());
    }
    Ok((framed.join("\n") + "\n").into_bytes())
}

pub(super) fn malloc_frame(output: &[u8]) -> Result<Vec<u8>, Error> {
    let text = std::str::from_utf8(output)?;
    let mut phases = BTreeMap::new();
    for line in text.lines().filter(|line| line.starts_with("PHASE ")) {
        let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
        let [_, name, micros, _] = fields.as_slice() else {
            return Err("staged malloc PHASE must have exactly name, us, and ok fields".into());
        };
        let micros = micros
            .strip_prefix("us=")
            .ok_or("staged malloc PHASE has no duration")?
            .parse::<u64>()?;
        if phases.insert(*name, micros).is_some() {
            return Err(format!("staged malloc phase {name} is duplicated").into());
        }
    }
    for phase in ["compute", "malloc"] {
        let micros = phases
            .get(phase)
            .ok_or_else(|| format!("staged malloc output is missing {phase}"))?;
        if *micros < MINIMUM_MALLOC_PHASE_MICROS {
            return Err(format!(
                "staged malloc phase {phase} is a smoke workload at {micros}us; minimum is {MINIMUM_MALLOC_PHASE_MICROS}us"
            )
            .into());
        }
    }
    frame(output)
}

pub(super) fn require_parity(workload: &str, native: &[u8], docker: &[u8]) -> Result<(), Error> {
    if native == docker {
        Ok(())
    } else {
        Err(format!("{workload} exact-output parity failed").into())
    }
}

pub(super) fn output_directory(workspace: &Path, requested: &Path) -> Result<PathBuf, Error> {
    let output = if requested.is_absolute() {
        requested.to_owned()
    } else {
        workspace.join(requested)
    };
    if output == workspace || !output.starts_with(workspace) || output.exists() {
        Err("benchmark stage output must be a new path beneath the workspace".into())
    } else {
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::super::profile::classified_failure;
    use super::{frame, malloc_frame, output_directory, require_parity};
    use crate::benchmark::definition::ArmSupport;
    use hl_process::Outcome;

    #[test]
    fn python_campaign_disables_bytecode_writes() {
        // Construction is integration-heavy; keep the invariant visible at its source too.
        let source = concat!(
            include_str!("../stage.rs"),
            include_str!("campaign.rs"),
            include_str!("profile.rs"),
            include_str!("output.rs")
        );
        assert!(source.matches("\"-B\"").count() >= 4);
        let python = include_str!("python.rs");
        assert!(python.matches("\"-B\"").count() >= 2);
    }

    #[test]
    fn exact_output_frame_changes_only_phase_time() {
        let output = b"META workload=malloc layout=plain version=1\nPHASE malloc us=42 ok=7\n";
        assert_eq!(
            frame(output).unwrap(),
            b"META workload=malloc layout=plain version=1\nPHASE malloc us=<time> ok=7\n"
        );
        assert!(frame(b"META x\r\n").is_err());
        assert!(frame(b"PHASE malloc us=42\n").is_err());
        assert!(frame(b"META x\nnoise\n").is_err());
        assert!(frame(b"META x\nMETA y\n").is_err());
    }

    #[test]
    fn malloc_stage_rejects_smoke_duration_and_requires_both_phases() {
        let valid =
            b"META workload=malloc layout=plain version=1\nPHASE compute us=5000 ok=7\nPHASE malloc us=5001 ok=8\n";
        assert!(malloc_frame(valid).is_ok());
        assert!(
            malloc_frame(
                b"META workload=malloc layout=plain version=1\nPHASE compute us=4999 ok=7\nPHASE malloc us=5001 ok=8\n"
            )
            .is_err()
        );
        assert!(malloc_frame(b"META workload=malloc layout=plain version=1\nPHASE compute us=5000 ok=7\n").is_err());
    }

    #[test]
    fn support_is_explicit_for_external_integrated_and_retained_arms() {
        let retained = ArmSupport::Incompatible {
            status: 1,
            stderr: "failure".into(),
            artifact_sha256: "a".repeat(64),
        };
        let support = std::collections::BTreeMap::from([
            ("E".into(), ArmSupport::Available),
            ("I".into(), ArmSupport::Available),
            ("R".into(), retained),
        ]);
        assert_eq!(support.keys().map(String::as_str).collect::<Vec<_>>(), ["E", "I", "R"]);
        assert!(matches!(support["E"], ArmSupport::Available));
        assert!(matches!(support["I"], ArmSupport::Available));
        assert!(matches!(support["R"], ArmSupport::Incompatible { .. }));
    }

    #[test]
    fn retained_python_failure_requires_exact_exit_stderr_and_hash() {
        let support = classified_failure(Outcome::Exited(Some(1)), b"_PySys_Create: failed\n", "b".repeat(64)).unwrap();
        assert!(matches!(
            support,
            ArmSupport::Incompatible { status: 1, ref stderr, ref artifact_sha256 }
                if stderr == "_PySys_Create: failed" && artifact_sha256 == &"b".repeat(64)
        ));
        assert!(classified_failure(Outcome::Exited(Some(0)), b"failure", "b".repeat(64)).is_err());
        assert!(classified_failure(Outcome::Exited(Some(1)), b"", "b".repeat(64)).is_err());
    }

    #[test]
    fn checksum_difference_refuses_cross_provider_parity() {
        assert!(
            require_parity(
                "malloc/plain",
                b"PHASE malloc us=<time> ok=7\n",
                b"PHASE malloc us=<time> ok=8\n"
            )
            .is_err()
        );
        require_parity("malloc/plain", b"same\n", b"same\n").unwrap();
    }

    #[test]
    fn stage_requires_a_new_workspace_owned_destination() {
        let workspace = tempfile::tempdir().unwrap();
        assert_eq!(
            output_directory(workspace.path(), std::path::Path::new("target/new-stage")).unwrap(),
            workspace.path().join("target/new-stage")
        );
        assert!(output_directory(workspace.path(), workspace.path()).is_err());
        assert!(output_directory(workspace.path(), workspace.path().parent().unwrap()).is_err());
        let existing = workspace.path().join("existing");
        std::fs::create_dir(&existing).unwrap();
        assert!(output_directory(workspace.path(), &existing).is_err());
    }
}
