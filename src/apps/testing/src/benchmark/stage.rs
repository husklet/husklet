use super::definition::artifact_identity;
use crate::{platform::HostProcess, suite::Error};
use clap::Args;
use hl_process::Outcome;
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

const IMAGE: &str = "alpine@sha256:14358309a308569c32bdc37e2e0e9694be33a9d99e68afb0f5ff33cc1f695dce";
const IMAGE_ID: &str = "sha256:14358309a308569c32bdc37e2e0e9694be33a9d99e68afb0f5ff33cc1f695dce";
const MAC: &str = "/usr/local/bin/mac";
const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Args)]
pub(crate) struct Options {
    /// New machine-local artifact directory beneath the repository workspace.
    #[arg(long)]
    output: PathBuf,
    /// Linux x86-64 static C compiler.
    #[arg(long, default_value = "x86_64-linux-gnu-gcc")]
    linux_cc: PathBuf,
}

pub(super) fn run(options: Options) -> Result<(), Error> {
    let workspace = crate::runtime::workspace()?;
    let output = stage_output(&workspace, &options.output)?;
    let source = workspace.join("tests/benchmark/three-arm/malloc_plain.c");
    let rootfs = output.join("rootfs");
    let linux = rootfs.join("benchmark/malloc-plain");
    let native = output.join("native/malloc-plain");
    let arch = output.join("tools/arch");
    let docker = output.join("tools/docker");
    fs::create_dir_all(linux.parent().ok_or("Linux guest has no parent")?)?;
    fs::create_dir_all(native.parent().ok_or("native guest has no parent")?)?;
    fs::create_dir_all(arch.parent().ok_or("tool has no parent")?)?;

    checked(
        &options.linux_cc,
        &[
            "-O3".into(),
            "-static".into(),
            source.display().to_string(),
            "-o".into(),
            linux.display().to_string(),
        ],
    )?;
    mac(&[
        "/mnt/mac/usr/bin/clang".into(),
        "-O3".into(),
        "-arch".into(),
        "x86_64".into(),
        mac_path(&source),
        "-o".into(),
        mac_path(&native),
    ])?;
    mac(&["cp".into(), "/mnt/mac/usr/bin/arch".into(), mac_path(&arch)])?;
    mac(&["cp".into(), "/mnt/mac/usr/local/bin/docker".into(), mac_path(&docker)])?;

    let inspect = mac(&[
        mac_path(&docker),
        "image".into(),
        "inspect".into(),
        IMAGE.into(),
        "--format".into(),
        "{{.Id}}".into(),
    ])?;
    if String::from_utf8(inspect)?.trim() != IMAGE_ID {
        return Err("pinned Docker image identity mismatch".into());
    }
    let native_output = mac(&[mac_path(&arch), "-x86_64".into(), mac_path(&native)])?;
    let docker_output = mac(&[
        mac_path(&docker),
        "run".into(),
        "--rm".into(),
        "--platform".into(),
        "linux/amd64".into(),
        "--mount".into(),
        format!(
            "type=bind,source={},target={},readonly",
            mac_path(&rootfs),
            rootfs.display()
        ),
        IMAGE.into(),
        linux.display().to_string(),
    ])?;
    let native_frame = frame(&native_output)?;
    let docker_frame = frame(&docker_output)?;
    require_parity(&native_frame, &docker_frame)?;
    fs::write(output.join("native.out"), native_output)?;
    fs::write(output.join("docker.out"), docker_output)?;
    fs::write(output.join("exact-output.frame"), &native_frame)?;
    let mut identities = String::from("artifact\tidentity\n");
    for path in [&rootfs, &linux, &native, &arch, &docker] {
        identities.push_str(&format!("{}\t{}\n", path.display(), artifact_identity(path)?));
    }
    identities.push_str(&format!("docker-image\t{IMAGE_ID}\n"));
    fs::write(output.join("artifacts.tsv"), identities)?;
    fs::write(output.join("BLOCKERS.txt"), blockers())?;
    println!(
        "READY malloc/plain\nBLOCKED campaign: see {}/BLOCKERS.txt",
        output.display()
    );
    Ok(())
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

fn mac(arguments: &[String]) -> Result<Vec<u8>, Error> {
    checked(Path::new(MAC), arguments)
}

fn mac_path(path: &Path) -> String {
    format!("/mnt/mac{}", path.display())
}

fn frame(output: &[u8]) -> Result<Vec<u8>, Error> {
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

fn require_parity(native: &[u8], docker: &[u8]) -> Result<(), Error> {
    if native == docker {
        Ok(())
    } else {
        Err("malloc/plain exact-output parity failed".into())
    }
}

fn blockers() -> &'static str {
    "Campaign not emitted: the strict schema requires real malloc/python/sqlite workloads across plain/sqlite layouts.\nAvailable and exact-output matched: malloc/plain Linux x86_64 ELF and x86_64 Mach-O.\nMissing: malloc/sqlite, python/plain, python/sqlite, sqlite/sqlite paired artifacts and their declared phases.\nMissing: selected, built Husklet x86 command profile and its smoke proof.\nPinned Docker image: alpine@sha256:14358309a308569c32bdc37e2e0e9694be33a9d99e68afb0f5ff33cc1f695dce.\n"
}

fn stage_output(workspace: &Path, requested: &Path) -> Result<PathBuf, Error> {
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
    use super::{blockers, frame, require_parity, stage_output};

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
    fn incomplete_stage_refuses_to_claim_a_campaign() {
        let text = blockers();
        assert!(text.starts_with("Campaign not emitted"));
        for missing in [
            "malloc/sqlite",
            "python/plain",
            "python/sqlite",
            "sqlite/sqlite",
            "Husklet x86",
        ] {
            assert!(text.contains(missing));
        }
    }

    #[test]
    fn checksum_difference_refuses_cross_provider_parity() {
        assert!(require_parity(b"PHASE malloc us=<time> ok=7\n", b"PHASE malloc us=<time> ok=8\n").is_err());
        require_parity(b"same\n", b"same\n").unwrap();
    }

    #[test]
    fn stage_requires_a_new_workspace_owned_destination() {
        let workspace = tempfile::tempdir().unwrap();
        assert_eq!(
            stage_output(workspace.path(), std::path::Path::new("target/new-stage")).unwrap(),
            workspace.path().join("target/new-stage")
        );
        assert!(stage_output(workspace.path(), workspace.path()).is_err());
        assert!(stage_output(workspace.path(), workspace.path().parent().unwrap()).is_err());
        let existing = workspace.path().join("existing");
        std::fs::create_dir(&existing).unwrap();
        assert!(stage_output(workspace.path(), &existing).is_err());
    }
}
