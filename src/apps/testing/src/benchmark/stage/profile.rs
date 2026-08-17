use super::{ArmSupport, Error, Outcome, Path, PathBuf, fs, malloc, python, sqlite};
use super::{
    capture_rootfs_guest, husklet_rootfs_guest, mac, mac_path, malloc_frame, native_library, raw_sha256, require_parity,
};
pub(super) struct RetainedProfile {
    pub(super) command: PathBuf,
    pub(super) python_failure: ArmSupport,
}

impl RetainedProfile {
    pub(super) fn stage(
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

pub(super) fn classified_failure(
    outcome: Outcome,
    stderr: &[u8],
    artifact_sha256: String,
) -> Result<ArmSupport, Error> {
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

pub(super) struct HuskletProfile {
    pub(super) primary: HuskletBuild,
    pub(super) independent_null: HuskletBuild,
    pub(super) source_identity: String,
    pub(super) toolchain_identity: String,
}

pub(super) struct HuskletBuild {
    pub(super) command: PathBuf,
    pub(super) library: PathBuf,
    pub(super) receipt: String,
    pub(super) build_command: Vec<String>,
}

impl HuskletProfile {
    pub(super) fn stage(workspace: &Path, output: &Path, cargo: &Path) -> Result<Self, Error> {
        let source_identity = super::artifact_identity(&workspace.join("src"))?;
        let mut toolchain = crate::record::FramedIdentity::new(b"husklet-benchmark-macos-toolchain-v1")?;
        for command in [
            vec![cargo.display().to_string(), "-Vv".into()],
            vec!["rustc".into(), "-Vv".into()],
            vec!["/mnt/mac/usr/bin/clang".into(), "--version".into()],
        ] {
            toolchain.field(&mac(&command)?)?;
        }
        Ok(Self {
            primary: Self::stage_build("primary", workspace, output, cargo)?,
            independent_null: Self::stage_build("independent-null", workspace, output, cargo)?,
            source_identity,
            toolchain_identity: toolchain.finish(),
        })
    }

    fn stage_build(label: &str, workspace: &Path, output: &Path, cargo: &Path) -> Result<HuskletBuild, Error> {
        let build = output.join(format!("husklet-build-{label}"));
        let build_command = vec![
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
        ];
        mac(&build_command)?;

        let built_command = build.join("release/hl-x86_64");
        let built_library = native_library(&build)?;
        let profile = output.join(format!("husklet-x86_64-macos-{label}"));
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
        Ok(HuskletBuild {
            command,
            library,
            receipt,
            build_command,
        })
    }
}
