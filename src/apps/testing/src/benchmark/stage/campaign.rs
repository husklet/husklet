use super::*;

pub(super) fn merge_rootfs(source: &Path, destination: &Path) -> Result<(), Error> {
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

pub(super) fn campaign(
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
    let support = |retained| {
        BTreeMap::from([
            ("E".into(), ArmSupport::Available),
            ("I".into(), ArmSupport::Available),
            ("R".into(), retained),
        ])
    };
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
                    timeout_seconds: 600,
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
                    timeout_seconds: 1_800,
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
                    timeout_seconds: 600,
                    wall_time: false,
                },
            ),
        ]),
        invariant_phases: vec!["compute".into()],
    })
}
