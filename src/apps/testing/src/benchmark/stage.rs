use super::definition::artifact_identity;
use crate::{platform::HostProcess, suite::Error};
use clap::Args;
use hl_process::Outcome;
use sha2::{Digest as _, Sha256};
use std::{
    fmt::Write as _,
    fs,
    io::Read as _,
    path::{Path, PathBuf},
    time::Duration,
};

mod malloc;
#[path = "stage/python.rs"]
mod python;
#[path = "stage/sqlite.rs"]
mod sqlite;

const IMAGE: &str = "alpine@sha256:14358309a308569c32bdc37e2e0e9694be33a9d99e68afb0f5ff33cc1f695dce";
const IMAGE_ID: &str = "sha256:14358309a308569c32bdc37e2e0e9694be33a9d99e68afb0f5ff33cc1f695dce";
const MAC: &str = "/usr/local/bin/mac";
const TIMEOUT: Duration = Duration::from_secs(30);
const PYTHON_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Args)]
pub(crate) struct Options {
    /// New machine-local artifact directory beneath the repository workspace.
    #[arg(long)]
    output: PathBuf,
    /// Cargo executable available on the macOS host.
    #[arg(long, default_value = "cargo")]
    mac_cargo: PathBuf,
}

pub(super) fn run(options: Options) -> Result<(), Error> {
    let workspace = crate::runtime::workspace()?;
    let output = stage_output(&workspace, &options.output)?;
    let source = workspace.join(malloc::SOURCE);
    let rootfs = output.join("rootfs");
    let arch = output.join("tools/arch");
    let docker = output.join("tools/docker");
    fs::create_dir_all(rootfs.join("benchmark"))?;
    fs::create_dir_all(output.join("native"))?;
    fs::create_dir_all(arch.parent().ok_or("tool has no parent")?)?;

    let layouts = malloc::layouts(&source, &rootfs, &output);
    for layout in &layouts {
        mac(&layout.native_arguments)?;
    }
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
    for layout in &layouts {
        malloc::build_linux(layout, &source, &rootfs, &docker)?;
    }
    let python_inspect = mac(&[
        mac_path(&docker),
        "image".into(),
        "inspect".into(),
        python::IMAGE.into(),
        "--format".into(),
        "{{.Id}}".into(),
    ])?;
    if String::from_utf8(python_inspect)?.trim() != python::IMAGE_ID {
        return Err("pinned Python Docker image identity mismatch".into());
    }
    let python = python::PythonProfile::stage(&output, &docker, &arch)?;
    let sqlite = sqlite::SqliteProfile::stage(&output, &docker, &arch)?;
    let husklet = HuskletProfile::stage(&workspace, &output, &options.mac_cargo)?;
    let python_husklet = PythonHusklet::stage(&output, &docker, &husklet.command)?;
    let sqlite_husklet = sqlite::SqliteProfile::stage_husklet(
        &output,
        &docker,
        &husklet.command,
        &output.join("sqlite-exact-output.frame"),
    )?;
    let mut identities = String::from("artifact\tidentity\n");
    for path in [
        &rootfs,
        &arch,
        &docker,
        &python.interpreter,
        &python_husklet.interpreter,
        &sqlite_husklet.rootfs,
        &sqlite_husklet.interpreter,
        &sqlite.command,
        &husklet.command,
        &husklet.library,
    ] {
        identities.push_str(&format!("{}\t{}\n", path.display(), artifact_identity(path)?));
    }
    identities.push_str(&format!("python-sqlite\t{}\n", python.sqlite_identity));
    identities.push_str(&format!("linux-sqlite\t{}\n", sqlite.linux_identity));
    for layout in &layouts {
        let native_output = mac(&[mac_path(&arch), "-x86_64".into(), mac_path(&layout.native)])?;
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
            layout.linux.display().to_string(),
        ])?;
        let native_frame = frame(&native_output)?;
        let docker_frame = frame(&docker_output)?;
        require_parity(&format!("malloc/{}", layout.name), &native_frame, &docker_frame)?;
        if layout.name == "plain" {
            let husklet_output = husklet_guest(&husklet.command, &layout.linux)?;
            let husklet_frame = frame(&husklet_output)?;
            require_parity("malloc/plain Husklet", &native_frame, &husklet_frame)?;
            fs::write(output.join("husklet-plain.out"), husklet_output)?;
            fs::write(output.join("exact-output-husklet-plain.frame"), husklet_frame)?;
        }
        fs::write(output.join(format!("native-{}.out", layout.name)), native_output)?;
        fs::write(output.join(format!("docker-{}.out", layout.name)), docker_output)?;
        fs::write(
            output.join(format!("exact-output-{}.frame", layout.name)),
            &native_frame,
        )?;
        for path in [&layout.linux, &layout.native] {
            identities.push_str(&format!("{}\t{}\n", path.display(), artifact_identity(path)?));
        }
    }
    identities.push_str(&format!("docker-image\t{IMAGE_ID}\n"));
    identities.push_str(&format!("python-docker-image\t{}\n", python::IMAGE_ID));
    fs::write(output.join("artifacts.tsv"), identities)?;
    fs::write(
        output.join("husklet-command.tsv"),
        format!(
            "command\t{}\nhost-architecture\taarch64-apple-darwin\nguest-architecture\tx86_64-linux\nsmoke\t--backend-receipt\nreceipt\t{}\n",
            husklet.command.display(),
            husklet.receipt
        ),
    )?;
    fs::write(output.join("BLOCKERS.txt"), blockers())?;
    println!(
        "READY malloc/plain malloc/sqlite python/plain python/sqlite sqlite/sqlite husklet/arm64-macos-x86_64-guest-malloc-python-sqlite\nBLOCKED campaign: see {}/BLOCKERS.txt",
        output.display()
    );
    Ok(())
}

struct HuskletProfile {
    command: PathBuf,
    library: PathBuf,
    receipt: String,
}

impl HuskletProfile {
    fn stage(workspace: &Path, output: &Path, cargo: &Path) -> Result<Self, Error> {
        let build = output.join("husklet-build");
        mac(&[
            "env".into(),
            "HL_NATIVE_COMPILE_CHECK=1".into(),
            "RUSTFLAGS=-C link-arg=-Wl,-rpath,@executable_path".into(),
            format!("CARGO_TARGET_DIR={}", mac_path(&build)),
            cargo.display().to_string(),
            "build".into(),
            "--quiet".into(),
            "--manifest-path".into(),
            mac_path(&workspace.join("Cargo.toml")),
            "--package".into(),
            "engine".into(),
            "--bin".into(),
            "hl-x86_64".into(),
            "--release".into(),
        ])?;

        let built_command = build.join("release/hl-x86_64");
        let built_library = native_library(&build)?;
        let profile = output.join("husklet-x86_64-macos");
        fs::create_dir(&profile)?;
        let command = profile.join("hl-x86_64");
        let library = profile.join("libhl_native_engine.dylib");
        // Publication is deliberately separate from the completed Cargo invocation.
        fs::copy(&built_command, &command)?;
        fs::copy(&built_library, &library)?;
        let slices = mac(&["/mnt/mac/usr/bin/lipo".into(), "-archs".into(), mac_path(&command)])?;
        if String::from_utf8(slices)?.split_ascii_whitespace().collect::<Vec<_>>() != ["arm64"] {
            return Err("Husklet profiling command is not a native arm64-only Mach-O".into());
        }
        let smoke = mac(&[mac_path(&command), "--backend-receipt".into()])?;
        let receipt = String::from_utf8(smoke)?.trim().to_owned();
        if !receipt
            .starts_with("{\"schema\":\"husklet-engine-backend-v1\",\"backend\":\"retained-c\",\"engine_sha256\":\"")
            || !receipt.ends_with("\"}")
        {
            return Err("native-arm64 Husklet x86 guest smoke emitted an invalid backend receipt".into());
        }
        let reported = receipt
            .strip_prefix("{\"schema\":\"husklet-engine-backend-v1\",\"backend\":\"retained-c\",\"engine_sha256\":\"")
            .and_then(|value| value.strip_suffix("\"}"))
            .ok_or("Husklet backend receipt framing changed")?;
        if reported != raw_sha256(&command)? {
            return Err("Husklet backend receipt is not bound to the staged command".into());
        }
        Ok(Self {
            command,
            library,
            receipt,
        })
    }
}

fn raw_sha256(path: &Path) -> Result<String, Error> {
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

fn native_library(build: &Path) -> Result<PathBuf, Error> {
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

fn mac(arguments: &[String]) -> Result<Vec<u8>, Error> {
    checked(Path::new(MAC), arguments)
}

fn husklet_guest(command: &Path, guest: &Path) -> Result<Vec<u8>, Error> {
    let arguments = [mac_path(command), mac_path(guest)];
    let captured = HostProcess::bounded_capture(Path::new(MAC), &arguments, TIMEOUT)?;
    if captured.outcome != Outcome::Exited(Some(0)) {
        return Err(format!(
            "native-arm64 Husklet x86 guest failed with {:?}: {}",
            captured.outcome,
            String::from_utf8_lossy(&captured.stderr)
        )
        .into());
    }
    let diagnostic = b"hl-test-displaced-et-exec: displaced\n";
    if !captured.stderr.is_empty() && captured.stderr != diagnostic {
        return Err(format!(
            "native-arm64 Husklet x86 guest wrote unexpected stderr: {}",
            String::from_utf8_lossy(&captured.stderr)
        )
        .into());
    }
    Ok(captured.stdout)
}

fn husklet_rootfs_guest(
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
    if captured.outcome != Outcome::Exited(Some(0)) || !captured.stderr.is_empty() {
        return Err(format!(
            "native-arm64 Husklet x86 rootfs guest failed with {:?}: {}",
            captured.outcome,
            String::from_utf8_lossy(&captured.stderr)
        )
        .into());
    }
    Ok(captured.stdout)
}

struct PythonHusklet {
    interpreter: PathBuf,
}

impl PythonHusklet {
    fn stage(output: &Path, docker: &Path, command: &Path) -> Result<Self, Error> {
        let rootfs = output.join("python-rootfs");
        let archive = output.join("python-rootfs.tar");
        fs::create_dir(&rootfs)?;
        let created = mac(&[
            mac_path(docker),
            "create".into(),
            "--platform".into(),
            "linux/amd64".into(),
            python::IMAGE.into(),
            "python3".into(),
            "-c".into(),
            "print(1)".into(),
        ])?;
        let container = String::from_utf8(created)?.trim().to_owned();
        mac(&[
            mac_path(docker),
            "export".into(),
            "--output".into(),
            mac_path(&archive),
            container.clone(),
        ])?;
        mac(&[mac_path(docker), "rm".into(), container])?;
        mac(&[
            "/mnt/mac/usr/bin/tar".into(),
            "-xf".into(),
            mac_path(&archive),
            "-C".into(),
            mac_path(&rootfs),
        ])?;
        fs::remove_file(&archive)?;
        let interpreter = rootfs.join("usr/local/bin/python3.12");
        let arguments = [
            mac_path(command),
            "--rootfs".into(),
            rootfs.display().to_string(),
            "usr/local/bin/python3.12".into(),
            "-c".into(),
            python::PLAIN_PROGRAM.into(),
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
        let husklet_frame = frame(&captured.stdout)?;
        require_parity("python/plain Husklet", &native_frame, &husklet_frame)?;
        fs::write(output.join("python-plain-husklet.out"), captured.stdout)?;
        fs::write(output.join("python-plain-husklet-exact-output.frame"), husklet_frame)?;
        Ok(Self { interpreter })
    }
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

fn require_parity(workload: &str, native: &[u8], docker: &[u8]) -> Result<(), Error> {
    if native == docker {
        Ok(())
    } else {
        Err(format!("{workload} exact-output parity failed").into())
    }
}

fn blockers() -> &'static str {
    "Campaign not emitted: the staged workloads are compatibility inputs, not timing evidence.\nAvailable and exact-output matched: malloc/plain, malloc/sqlite, python/plain, python/sqlite, and sqlite/sqlite on Linux x86_64 and x86_64 Mach-O.\nAvailable: a native-arm64 macOS Husklet command selecting the x86_64 Linux guest engine, with command-bound backend receipt and private library identities; malloc/plain, Python/plain, and sqlite/sqlite complete through that command with exact-output parity.\nMissing: Husklet execution validation for the remaining workloads (sqlite-linked malloc and Python/sqlite), balanced-order campaign execution with a unique ledger, null/control arms, sustained quiet, binary hashes, and host-load evidence.\nPinned Docker images: alpine@sha256:14358309a308569c32bdc37e2e0e9694be33a9d99e68afb0f5ff33cc1f695dce and python:3.12-alpine@sha256:6d43704baacd1bfbe7c295d7f13079d5d8104ed33568873133f8fc69980419df.\n"
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
        for missing in ["balanced-order", "unique ledger", "remaining workloads"] {
            assert!(text.contains(missing));
        }
        assert!(text.contains("native-arm64 macOS Husklet"));
        assert!(text.contains("Python/plain, and sqlite/sqlite complete"));
        assert!(text.contains("sqlite/sqlite complete through that command"));
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
