use super::definition::{
    Arm, ArmSupport, Artifact, CAMPAIGN_SCHEMA, Campaign, GuestPath, Layout as CampaignLayout, Workload,
    artifact_identity,
};
use crate::{
    platform::{HostProcess, ProcessCapture},
    suite::Error,
};
use clap::Args;
use hl_process::Outcome;
use sha2::{Digest as _, Sha256};
use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs,
    io::Read as _,
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

#[derive(Clone)]
struct WorkFactor(String);

impl WorkFactor {
    fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for WorkFactor {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let Some((first, second)) = value.split_once(',') else {
            return Err("work factor must be a pair such as 4,2".into());
        };
        if second.contains(',')
            || [first, second]
                .into_iter()
                .any(|part| !matches!(part, "1" | "2" | "4" | "8"))
        {
            return Err("each work factor must be one of 1,2,4,8".into());
        }
        Ok(Self(value.into()))
    }
}

mod malloc;
#[path = "stage/python.rs"]
mod python;
#[path = "stage/rootfs.rs"]
mod rootfs;
#[path = "stage/sqlite.rs"]
mod sqlite;

const IMAGE: &str = "alpine@sha256:14358309a308569c32bdc37e2e0e9694be33a9d99e68afb0f5ff33cc1f695dce";
const IMAGE_ID: &str = "sha256:14358309a308569c32bdc37e2e0e9694be33a9d99e68afb0f5ff33cc1f695dce";
const MAC: &str = "/usr/local/bin/mac";
const TIMEOUT: Duration = Duration::from_secs(30);
const PYTHON_TIMEOUT: Duration = Duration::from_secs(90);
const PREPARATION_COMPILE_TIMEOUT: Duration = Duration::from_secs(180);
const MINIMUM_MALLOC_PHASE_MICROS: u64 = 5_000;

#[derive(Args)]
pub(crate) struct Options {
    /// New machine-local artifact directory beneath the repository workspace.
    #[arg(long)]
    output: PathBuf,
    /// Cargo executable available on the macOS host.
    #[arg(long, default_value = "cargo")]
    mac_cargo: PathBuf,
    /// Previously built standalone retained-C x86-64 engine oracle.
    #[arg(long)]
    retained: PathBuf,
    /// Offline SQLite 3.50.1 amalgamation archive with the pinned content hash.
    #[arg(long)]
    sqlite_amalgamation: PathBuf,
    /// Compute,malloc work factors selected without rebuilding the guest.
    #[arg(long, default_value = "8,8")]
    malloc_factor: WorkFactor,
    /// Compute,codec work factors for the plain Python workload.
    #[arg(long, default_value = "4,4")]
    python_plain_factor: WorkFactor,
    /// Write,read work factors for the Python SQLite workload.
    #[arg(long, default_value = "4,2")]
    python_sqlite_factor: WorkFactor,
    /// Write,read work factors for the compiled SQLite workload.
    #[arg(long, default_value = "4,4")]
    sqlite_factor: WorkFactor,
}

pub(super) fn run(options: Options) -> Result<(), Error> {
    let workspace = crate::runtime::workspace()?;
    let output = stage_output(&workspace, &options.output)?;
    let source = workspace.join(malloc::SOURCE);
    let sqlite_source = workspace.join(sqlite::SOURCE);
    let rootfs = output.join("rootfs");
    let arch = output.join("tools/arch");
    let docker = output.join("tools/docker");
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
    rootfs::stage(&output, &docker)?;
    for layout in &layouts {
        malloc::build_linux(layout, &source, &rootfs, &docker)?;
    }
    let python = python::PythonProfile::stage(
        &output,
        &docker,
        &arch,
        options.python_plain_factor.as_str(),
        options.python_sqlite_factor.as_str(),
    )?;
    let sqlite = sqlite::SqliteProfile::stage(
        &output,
        &rootfs,
        &sqlite_source,
        &options.sqlite_amalgamation,
        &docker,
        &arch,
        options.sqlite_factor.as_str(),
    )?;
    let husklet = HuskletProfile::stage(&workspace, &output, &options.mac_cargo)?;
    let python_husklet =
        PythonHusklet::stage(&output, &rootfs, &husklet.command, options.python_plain_factor.as_str())?;
    let sqlite_husklet = sqlite::SqliteProfile::stage_husklet(
        &output,
        &rootfs,
        &husklet.command,
        &output.join("sqlite-exact-output.frame"),
        options.sqlite_factor.as_str(),
    )?;
    merge_rootfs(&python_husklet.rootfs, &rootfs)?;
    let retained = RetainedProfile::stage(
        &options.retained,
        &output,
        &rootfs,
        &layouts,
        options.malloc_factor.as_str(),
        options.python_plain_factor.as_str(),
        options.sqlite_factor.as_str(),
    )?;
    let mut identities = String::from("artifact\tidentity\n");
    for path in [
        &rootfs,
        &arch,
        &docker,
        &python.interpreter,
        &python_husklet.interpreter,
        &sqlite_husklet.interpreter,
        &sqlite.command,
        &husklet.command,
        &husklet.library,
    ] {
        identities.push_str(&format!("{}\t{}\n", path.display(), artifact_identity(path)?));
    }
    identities.push_str(&format!("python-sqlite\t{}\n", python.sqlite_identity));
    identities.push_str(&format!("linux-sqlite\t{}\n", sqlite.linux_identity));
    identities.push_str(&format!("sqlite-amalgamation\t{}\n", sqlite.source_identity));
    for layout in &layouts {
        let native_output = mac(&[
            mac_path(&arch),
            "-x86_64".into(),
            mac_path(&layout.native),
            options.malloc_factor.0.clone(),
        ])?;
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
            options.malloc_factor.0.clone(),
        ])?;
        let native_frame = malloc_frame(&native_output)?;
        let docker_frame = malloc_frame(&docker_output)?;
        require_parity(&format!("malloc/{}", layout.name), &native_frame, &docker_frame)?;
        if layout.name == "plain" {
            let guest = layout
                .linux
                .strip_prefix(&rootfs)?
                .to_str()
                .ok_or("malloc guest path is not UTF-8")?;
            let husklet_output =
                husklet_rootfs_guest(&husklet.command, &rootfs, guest, &[options.malloc_factor.as_str()])?;
            let husklet_frame = malloc_frame(&husklet_output)?;
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
    let campaign = campaign(
        &output,
        &rootfs,
        &arch,
        &layouts,
        &python,
        &sqlite,
        &husklet,
        &retained,
        &options.malloc_factor,
        &options.python_plain_factor,
        &options.python_sqlite_factor,
        &options.sqlite_factor,
    )?;
    let campaign_path = output.join("campaign.yaml");
    fs::write(&campaign_path, serde_yaml::to_string(&campaign)?)?;
    Campaign::load(&campaign_path)?.verify_artifacts()?;
    println!("READY campaign {}", campaign_path.display());
    Ok(())
}

fn merge_rootfs(source: &Path, destination: &Path) -> Result<(), Error> {
    mac(&[
        "/mnt/mac/bin/cp".into(),
        "-a".into(),
        format!("{}/.", mac_path(source)),
        mac_path(destination),
    ])?;
    Ok(())
}

fn artifact(path: &Path) -> Result<Artifact, Error> {
    Ok(Artifact {
        path: path.to_owned(),
        sha256: artifact_identity(path)?,
    })
}

fn support(retained: ArmSupport) -> BTreeMap<String, ArmSupport> {
    BTreeMap::from([
        ("E".into(), ArmSupport::Available),
        ("I".into(), ArmSupport::Available),
        ("R".into(), retained),
    ])
}

fn campaign(
    output: &Path,
    rootfs: &Path,
    arch: &Path,
    layouts: &[malloc::Layout],
    python: &python::PythonProfile,
    sqlite: &sqlite::SqliteProfile,
    integrated: &HuskletProfile,
    retained: &RetainedProfile,
    malloc_factor: &WorkFactor,
    python_plain_factor: &WorkFactor,
    python_sqlite_factor: &WorkFactor,
    sqlite_factor: &WorkFactor,
) -> Result<Campaign, Error> {
    let mac_proxy = output.join("tools/mac");
    fs::copy(MAC, &mac_proxy)?;
    let linux = |relative: &str| rootfs.join(relative);
    let host = |path: &Path| mac_path(path);
    let malloc_map = layouts
        .iter()
        .map(|layout| (layout.linux.clone(), layout.native.clone()));
    let guest_map = malloc_map
        .chain([
            (linux("usr/local/bin/python3.12"), python.interpreter.clone()),
            (sqlite.guest.clone(), sqlite.command.clone()),
        ])
        .collect();
    let mut external_artifacts = BTreeMap::from([
        ("command".into(), artifact(&mac_proxy)?),
        ("arch".into(), artifact(arch)?),
        ("python".into(), artifact(&python.interpreter)?),
        ("sqlite".into(), artifact(&sqlite.command)?),
    ]);
    for layout in layouts {
        external_artifacts.insert(format!("malloc-{}", layout.name), artifact(&layout.native)?);
    }
    let smoke_guest = host(&layouts[0].native);
    let rootfs_host = rootfs.display().to_string();
    let arms = BTreeMap::from([
        (
            "E".into(),
            Arm {
                command: vec![mac_proxy.display().to_string(), host(arch), "-x86_64".into()],
                artifacts: external_artifacts,
                smoke: vec![
                    mac_proxy.display().to_string(),
                    host(arch),
                    "-x86_64".into(),
                    smoke_guest,
                    malloc_factor.0.clone(),
                ],
                guest_path: GuestPath::HostAbsolute,
                guest_map,
            },
        ),
        (
            "I".into(),
            Arm {
                command: vec![
                    mac_proxy.display().to_string(),
                    host(&integrated.command),
                    "--rootfs".into(),
                    rootfs_host.clone(),
                ],
                artifacts: BTreeMap::from([
                    ("command".into(), artifact(&mac_proxy)?),
                    ("engine".into(), artifact(&integrated.command)?),
                    ("library".into(), artifact(&integrated.library)?),
                ]),
                smoke: vec![
                    mac_proxy.display().to_string(),
                    host(&integrated.command),
                    "--rootfs".into(),
                    rootfs_host.clone(),
                    "benchmark/malloc-plain".into(),
                    malloc_factor.0.clone(),
                ],
                guest_path: GuestPath::RootfsAbsolute,
                guest_map: BTreeMap::new(),
            },
        ),
        (
            "R".into(),
            Arm {
                command: vec![
                    mac_proxy.display().to_string(),
                    host(&retained.command),
                    "--rootfs".into(),
                    rootfs_host.clone(),
                ],
                artifacts: BTreeMap::from([
                    ("command".into(), artifact(&mac_proxy)?),
                    ("engine".into(), artifact(&retained.command)?),
                ]),
                smoke: vec![
                    mac_proxy.display().to_string(),
                    host(&retained.command),
                    "--rootfs".into(),
                    rootfs_host,
                    "benchmark/malloc-plain".into(),
                    malloc_factor.0.clone(),
                ],
                guest_path: GuestPath::RootfsAbsolute,
                guest_map: BTreeMap::new(),
            },
        ),
    ]);
    let available = || ArmSupport::Available;
    let malloc_support = || support(available());
    let python_support = || support(retained.python_failure.clone());
    Ok(Campaign {
        schema: CAMPAIGN_SCHEMA.into(),
        rounds: 4,
        samples_per_row: 3,
        rootfs: artifact(rootfs)?,
        arms,
        layouts: BTreeMap::from([
            (
                "plain".into(),
                CampaignLayout {
                    phases: vec!["compute".into(), "malloc".into()],
                },
            ),
            (
                "sqlite".into(),
                CampaignLayout {
                    phases: vec![
                        "compute".into(),
                        "malloc".into(),
                        "sqlite-write".into(),
                        "sqlite-read".into(),
                    ],
                },
            ),
        ]),
        workloads: BTreeMap::from([
            (
                "malloc".into(),
                Workload {
                    commands: BTreeMap::from([
                        (
                            "plain".into(),
                            vec![
                                linux("benchmark/malloc-plain").display().to_string(),
                                malloc_factor.0.clone(),
                            ],
                        ),
                        (
                            "sqlite".into(),
                            vec![
                                linux("benchmark/malloc-sqlite").display().to_string(),
                                malloc_factor.0.clone(),
                            ],
                        ),
                    ]),
                    layout_phases: BTreeMap::from([
                        ("plain".into(), vec!["compute".into(), "malloc".into()]),
                        ("sqlite".into(), vec!["compute".into(), "malloc".into()]),
                    ]),
                    arm_support: BTreeMap::from([
                        ("plain".into(), malloc_support()),
                        ("sqlite".into(), malloc_support()),
                    ]),
                    phases: vec!["compute".into(), "malloc".into()],
                    timeout_seconds: 30,
                    wall_time: false,
                },
            ),
            (
                "python".into(),
                Workload {
                    commands: BTreeMap::from([
                        (
                            "plain".into(),
                            vec![
                                linux("usr/local/bin/python3.12").display().to_string(),
                                "-B".into(),
                                "-c".into(),
                                python::PLAIN_PROGRAM.into(),
                                python_plain_factor.0.clone(),
                            ],
                        ),
                        (
                            "sqlite".into(),
                            vec![
                                linux("usr/local/bin/python3.12").display().to_string(),
                                "-B".into(),
                                "-c".into(),
                                python::SQLITE_PROGRAM.into(),
                                python_sqlite_factor.0.clone(),
                            ],
                        ),
                    ]),
                    layout_phases: BTreeMap::from([
                        ("plain".into(), vec!["python-compute".into(), "python-codec".into()]),
                        (
                            "sqlite".into(),
                            vec!["python-sqlite-write".into(), "python-sqlite-read".into()],
                        ),
                    ]),
                    arm_support: BTreeMap::from([
                        ("plain".into(), python_support()),
                        ("sqlite".into(), python_support()),
                    ]),
                    phases: vec![
                        "python-compute".into(),
                        "python-codec".into(),
                        "python-sqlite-write".into(),
                        "python-sqlite-read".into(),
                    ],
                    timeout_seconds: 90,
                    wall_time: false,
                },
            ),
            (
                "sqlite".into(),
                Workload {
                    commands: BTreeMap::from([(
                        "sqlite".into(),
                        vec![sqlite.guest.display().to_string(), sqlite_factor.0.clone()],
                    )]),
                    layout_phases: BTreeMap::from([(
                        "sqlite".into(),
                        vec!["sqlite-write".into(), "sqlite-read".into()],
                    )]),
                    arm_support: BTreeMap::from([("sqlite".into(), support(available()))]),
                    phases: vec!["sqlite-write".into(), "sqlite-read".into()],
                    timeout_seconds: 30,
                    wall_time: false,
                },
            ),
        ]),
        invariant_phases: vec!["compute".into()],
    })
}

struct RetainedProfile {
    command: PathBuf,
    python_failure: ArmSupport,
}

impl RetainedProfile {
    fn stage(
        source: &Path,
        output: &Path,
        rootfs: &Path,
        layouts: &[malloc::Layout],
        malloc_factor: &str,
        python_factor: &str,
        sqlite_factor: &str,
    ) -> Result<Self, Error> {
        if !source.is_absolute() || !source.is_file() {
            return Err("--retained must name an absolute regular file".into());
        }
        let command = output.join("retained/hl-engine-linux-x86_64");
        fs::create_dir(command.parent().ok_or("retained command has no parent")?)?;
        fs::copy(source, &command)?;
        let smoke = husklet_rootfs_guest(&command, rootfs, "benchmark/malloc-plain", &[malloc_factor])?;
        let plain_native = mac(&[mac_path(&layouts[0].native), malloc_factor.into()])?;
        require_parity(
            "malloc/plain retained",
            &malloc_frame(&plain_native)?,
            &malloc_frame(&smoke)?,
        )?;
        let python = capture_rootfs_guest(
            &command,
            rootfs,
            "usr/local/bin/python3.12",
            &["-B", "-c", python::PLAIN_PROGRAM, python_factor],
        )?;
        let artifact_sha256 = raw_sha256(&rootfs.join("usr/local/bin/python3.12"))?;
        let python_failure = classified_failure(python.outcome, &python.stderr, artifact_sha256)?;
        for layout in layouts {
            let output_bytes = husklet_rootfs_guest(
                &command,
                rootfs,
                &format!("benchmark/malloc-{}", layout.name),
                &[malloc_factor],
            )?;
            let native = mac(&[mac_path(&layout.native), malloc_factor.into()])?;
            require_parity(
                &format!("malloc/{} retained", layout.name),
                &malloc_frame(&native)?,
                &malloc_frame(&output_bytes)?,
            )?;
        }
        let sqlite_output = husklet_rootfs_guest(&command, rootfs, "benchmark/sqlite", &[sqlite_factor])?;
        require_parity(
            "sqlite/sqlite retained",
            &fs::read(output.join("sqlite-exact-output.frame"))?,
            &sqlite::profile_frame(&sqlite_output)?,
        )?;
        Ok(Self {
            command,
            python_failure,
        })
    }
}

fn classified_failure(outcome: Outcome, stderr: &[u8], artifact_sha256: String) -> Result<ArmSupport, Error> {
    let status = match outcome {
        Outcome::Exited(Some(status)) if status != 0 => status,
        other => return Err(format!("retained Python failure was not a nonzero exit: {other:?}").into()),
    };
    if stderr.is_empty() {
        return Err("retained Python failure did not emit stderr".into());
    }
    Ok(ArmSupport::Incompatible {
        status,
        stderr: String::from_utf8(stderr.to_vec())?.trim().to_owned(),
        artifact_sha256,
    })
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

pub(super) fn mac_preparation_compile(arguments: &[String]) -> Result<Vec<u8>, Error> {
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

fn capture_rootfs_guest(
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

struct PythonHusklet {
    interpreter: PathBuf,
    rootfs: PathBuf,
}

impl PythonHusklet {
    fn stage(output: &Path, rootfs: &Path, command: &Path, factor: &str) -> Result<Self, Error> {
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

fn malloc_frame(output: &[u8]) -> Result<Vec<u8>, Error> {
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

fn require_parity(workload: &str, native: &[u8], docker: &[u8]) -> Result<(), Error> {
    if native == docker {
        Ok(())
    } else {
        Err(format!("{workload} exact-output parity failed").into())
    }
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
    use super::{classified_failure, frame, malloc_frame, require_parity, stage_output, support};
    use crate::benchmark::definition::ArmSupport;
    use hl_process::Outcome;

    #[test]
    fn python_campaign_disables_bytecode_writes() {
        // Construction is integration-heavy; keep the invariant visible at its source too.
        let source = include_str!("stage.rs");
        assert!(source.matches("\"-B\".into()").count() >= 3);
        let python = include_str!("stage/python.rs");
        assert!(python.matches("\"-B\".into()").count() >= 3);
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
        let support = support(retained);
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
