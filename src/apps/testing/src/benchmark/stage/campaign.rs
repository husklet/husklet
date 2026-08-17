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
    let mac_proxy_null = output.join("tools/mac-independent-null");
    fs::copy(MAC, &mac_proxy)?;
    fs::copy(MAC, &mac_proxy_null)?;
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
    let null_malloc_map = layouts
        .iter()
        .map(|layout| (layout.linux.clone(), layout.native_null.clone()));
    let null_guest_map = null_malloc_map
        .chain([
            (linux("usr/local/bin/python3.12"), python.null_interpreter.clone()),
            (sqlite.guest.clone(), sqlite.null_command.clone()),
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
    let mut external_null_artifacts = BTreeMap::from([
        ("command".into(), artifact(&mac_proxy_null)?),
        ("arch".into(), artifact(arch)?),
        ("python".into(), artifact(&python.null_interpreter)?),
        ("sqlite".into(), artifact(&sqlite.null_command)?),
    ]);
    for layout in layouts {
        external_null_artifacts.insert(format!("malloc-{}", layout.name), artifact(&layout.native_null)?);
    }
    let primary_native_builds = layouts
        .iter()
        .map(|layout| layout.native_arguments.clone())
        .chain(sqlite.primary_build.iter().cloned())
        .collect::<Vec<_>>();
    let null_native_builds = layouts
        .iter()
        .map(|layout| layout.native_null_arguments.clone())
        .chain(sqlite.null_build.iter().cloned())
        .collect::<Vec<_>>();
    let smoke_guest = host(&layouts[0].native);
    let rootfs_host = rootfs.display().to_string();
    let arms = BTreeMap::from([
        (
            "E".into(),
            Arm {
                primary: Profile {
                    command: vec![mac_proxy.display().to_string(), host(arch), "-x86_64".into()],
                    build: receipt(&external_artifacts, &primary_native_builds)?,
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
                independent_null: Some(Profile {
                    command: vec![mac_proxy_null.display().to_string(), host(arch), "-x86_64".into()],
                    build: receipt(&external_null_artifacts, &null_native_builds)?,
                    artifacts: external_null_artifacts,
                    smoke: vec![
                        mac_proxy_null.display().to_string(),
                        host(arch),
                        "-x86_64".into(),
                        host(&layouts[0].native_null),
                        malloc_factor.0.clone(),
                    ],
                    guest_path: GuestPath::HostAbsolute,
                    guest_map: null_guest_map,
                }),
                null_unqualified_reason: None,
            },
        ),
        (
            "I".into(),
            Arm {
                primary: integrated_profile(
                    &mac_proxy,
                    &rootfs_host,
                    &integrated.primary,
                    &integrated.source_identity,
                    &integrated.toolchain_identity,
                    malloc_factor,
                )?,
                independent_null: Some(integrated_profile(
                    &mac_proxy,
                    &rootfs_host,
                    &integrated.independent_null,
                    &integrated.source_identity,
                    &integrated.toolchain_identity,
                    malloc_factor,
                )?),
                null_unqualified_reason: None,
            },
        ),
        (
            "R".into(),
            Arm {
                primary: Profile {
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
                        rootfs_host.clone(),
                        "benchmark/malloc-plain".into(),
                        malloc_factor.0.clone(),
                    ],
                    guest_path: GuestPath::RootfsAbsolute,
                    guest_map: BTreeMap::new(),
                    build: retained_receipt(&retained.command)?,
                },
                independent_null: None,
                null_unqualified_reason: Some(
                    "retained oracle was supplied as a binary without a reproducible build recipe".into(),
                ),
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

fn receipt(artifacts: &BTreeMap<String, Artifact>, commands: &[Vec<String>]) -> Result<BuildReceipt, Error> {
    let outputs = artifacts
        .iter()
        // The system Python executable is copied, not independently rebuilt.
        // Keep it hashed as an execution artifact but do not qualify it as a
        // binary-build null output.
        .filter(|(name, _)| !matches!(name.as_str(), "command" | "arch" | "python"))
        .map(|(name, artifact)| (name.clone(), artifact.sha256.clone()))
        .collect();
    let workspace = crate::runtime::workspace()?;
    Ok(BuildReceipt {
        command: commands
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()?,
        inputs: BTreeMap::from([
            (
                "clang-toolchain".into(),
                artifact_identity(Path::new("/mnt/mac/usr/bin/clang"))?,
            ),
            (
                "malloc-source".into(),
                artifact_identity(&workspace.join(malloc::SOURCE))?,
            ),
            (
                "sqlite-fixture-source".into(),
                artifact_identity(&workspace.join(sqlite::SOURCE))?,
            ),
            (
                "python-workload-source".into(),
                crate::record::FramedIdentity::of(
                    format!("{}\n{}", python::PLAIN_PROGRAM, python::SQLITE_PROGRAM).as_bytes(),
                ),
            ),
        ]),
        outputs,
    })
}

fn integrated_profile(
    mac_proxy: &Path,
    rootfs_host: &str,
    build: &HuskletBuild,
    source_identity: &str,
    toolchain_identity: &str,
    malloc_factor: &WorkFactor,
) -> Result<Profile, Error> {
    let artifacts: BTreeMap<String, Artifact> = BTreeMap::from([
        ("command".into(), artifact(mac_proxy)?),
        ("engine".into(), artifact(&build.command)?),
        ("library".into(), artifact(&build.library)?),
    ]);
    let mut receipt = BuildReceipt {
        command: build.build_command.clone(),
        inputs: BTreeMap::new(),
        outputs: artifacts
            .iter()
            .filter(|(name, _)| matches!(name.as_str(), "engine" | "library"))
            .map(|(name, artifact)| (name.clone(), artifact.sha256.clone()))
            .collect(),
    };
    // Both independent builds consume the same immutable workspace contract.
    receipt.inputs = BTreeMap::from([
        (
            "cargo-manifest".into(),
            artifact_identity(&crate::runtime::workspace()?.join("Cargo.toml"))?,
        ),
        (
            "cargo-lock".into(),
            artifact_identity(&crate::runtime::workspace()?.join("Cargo.lock"))?,
        ),
        ("workspace-source".into(), source_identity.into()),
        ("toolchain".into(), toolchain_identity.into()),
    ]);
    Ok(Profile {
        command: vec![
            mac_proxy.display().to_string(),
            mac_path(&build.command),
            "--rootfs".into(),
            rootfs_host.into(),
        ],
        artifacts,
        smoke: vec![
            mac_proxy.display().to_string(),
            mac_path(&build.command),
            "--rootfs".into(),
            rootfs_host.into(),
            "benchmark/malloc-plain".into(),
            malloc_factor.0.clone(),
        ],
        guest_path: GuestPath::RootfsAbsolute,
        guest_map: BTreeMap::new(),
        build: receipt,
    })
}

fn retained_receipt(command: &Path) -> Result<BuildReceipt, Error> {
    let digest = artifact_identity(command)?;
    Ok(BuildReceipt {
        command: vec!["supplied-prebuilt-retained-oracle".into()],
        inputs: BTreeMap::from([("supplied-binary".into(), digest.clone())]),
        outputs: BTreeMap::from([("engine".into(), digest)]),
    })
}
