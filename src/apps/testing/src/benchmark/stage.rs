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
    time::Duration,
};

#[path = "stage/factor.rs"]
mod factor;
mod malloc;
#[path = "stage/python.rs"]
mod python;
#[path = "stage/rootfs.rs"]
mod rootfs;
#[path = "stage/sqlite.rs"]
mod sqlite;

use factor::WorkFactor;

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
    let output = output_directory(&workspace, &options.output)?;
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

use campaign::{campaign, merge_rootfs};

#[path = "stage/campaign.rs"]
mod campaign;

use profile::{HuskletProfile, RetainedProfile};

#[path = "stage/profile.rs"]
mod profile;

use output::{
    PythonHusklet, capture_rootfs_guest, frame, husklet_rootfs_guest, mac, mac_path, malloc_frame, native_library,
    output_directory, raw_sha256, require_parity,
};

pub(super) use output::mac_preparation_compile;

#[path = "stage/output.rs"]
mod output;
