//! Process adapters for the packaged engine executables.

use clap::Parser;
use sha2::{Digest, Sha256};
use std::io::Read as _;
use std::path::PathBuf;

/// The fixed guest architecture selected by a worker executable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Guest {
    Aarch64,
    X86_64,
}

impl Guest {
    const fn name(self) -> &'static str {
        match self {
            Self::Aarch64 => "aarch64",
            Self::X86_64 => "x86_64",
        }
    }

    const fn program(self) -> &'static str {
        match self {
            Self::Aarch64 => "hl-aarch64",
            Self::X86_64 => "hl-x86_64",
        }
    }

    const fn isa(self) -> hl_engine::activation::GuestIsa {
        match self {
            Self::Aarch64 => hl_engine::activation::GuestIsa::Aarch64,
            Self::X86_64 => hl_engine::activation::GuestIsa::X86_64,
        }
    }

    fn named(value: &str) -> Option<Self> {
        match value {
            "aarch64" | "arm64" => Some(Self::Aarch64),
            "x86_64" | "amd64" => Some(Self::X86_64),
            _ => None,
        }
    }
}

/// Runs one architecture-specific engine worker process.
pub struct Worker;

#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
enum MixedSseControl {
    On,
    Off,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
enum JccIbtcControl {
    On,
    Off,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
enum DirectJmpIbtcControl {
    On,
    Off,
}

#[derive(Parser)]
struct BackendReceiptArguments {
    #[arg(long = "guest-isa")]
    guest: Option<String>,
}

/// The engine workers are the developer-facing entry point to a guest, so every refusal they make
/// has to say what was wrong with the command line. `--help` and `--version` are part of that:
/// clap generates both, and a worker that discards them leaves a mistyped flag looking exactly
/// like a crash.
#[derive(Debug, Parser)]
#[command(
    version,
    trailing_var_arg = true,
    about = "Run one guest program under the Husklet execution engine.",
    long_about = "Run one guest program under the Husklet execution engine.\n\n\
                  With --rootfs the executable is a path inside the image and is resolved there, \
                  following the image's own symbolic links; without it the executable is a host \
                  path. Everything after the executable is passed to the guest unchanged."
)]
struct LaunchArguments {
    /// Guest ISA this run must use; a worker refuses an ISA it was not built to serve.
    #[arg(long = "guest-isa", value_name = "ISA")]
    guest: Option<String>,
    /// Print the guest's exit kind, status and detail to standard error when it finishes.
    #[arg(long)]
    report_exit: bool,
    /// Emit the exact executable and native-library paths and hashes opened by this process.
    #[arg(long)]
    loader_receipt: bool,
    /// Exact native engine artifact selected before construction.
    #[arg(long, value_name = "PATH")]
    native_library: Option<PathBuf>,
    /// Emit native-engine diagnostics, including the translation backend receipt.
    #[arg(long)]
    diagnostics: bool,
    /// Run supported x86-64 guest blocks through the experimental translation backend.
    #[arg(long)]
    translit: bool,
    /// Admit normal and SSE instructions into one bounded same-ISA descriptor.
    #[arg(long, value_enum, value_name = "on|off")]
    translit_mixed_sse: Option<MixedSseControl>,
    /// Late-link unresolved constant JCC targets through the same-ISA IBTC.
    #[arg(long, value_enum, value_name = "on|off")]
    translit_jcc_ibtc: Option<JccIbtcControl>,
    /// Late-link direct JMP targets through the same-ISA IBTC.
    #[arg(long, value_enum, value_name = "on|off")]
    translit_direct_jmp_ibtc: Option<DirectJmpIbtcControl>,
    /// Execute a same-ISA Linux x86-64 guest under the experimental native syscall supervisor.
    #[arg(long)]
    native_supervised: bool,
    /// Set one launch-scoped native test injection in a hooks-enabled worker.
    #[cfg(feature = "native-test-hooks")]
    #[arg(long, value_name = "KEY=VALUE", hide = true, value_parser = parse_native_test_option)]
    native_test_option: Vec<NativeTestOption>,
    /// Existing container root used to resolve the guest entry and `PT_INTERP`.
    #[arg(long)]
    rootfs: Option<PathBuf>,
    /// Guest entry: a path inside `--rootfs`, or a host path when no rootfs is given.
    executable: PathBuf,
    /// Arguments handed to the guest unchanged.
    #[arg(allow_hyphen_values = true)]
    arguments: Vec<String>,
}

#[cfg(feature = "native-test-hooks")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NativeTestOption {
    name: &'static str,
    value: &'static str,
}

#[cfg(feature = "native-test-hooks")]
fn parse_native_test_option(value: &str) -> Result<NativeTestOption, String> {
    match value {
        "HL_TRANSLIT_FS_AUTHORITY_TEST=1" => Ok(NativeTestOption {
            name: "HL_TRANSLIT_FS_AUTHORITY_TEST",
            value: "1",
        }),
        _ if !value.contains('=') => Err("native test options use KEY=VALUE syntax".to_owned()),
        _ => Err("unsupported native test option; expected HL_TRANSLIT_FS_AUTHORITY_TEST=1".to_owned()),
    }
}

/// Why a worker could not run the guest.
///
/// The two arms differ in who is at fault, and therefore in what the sentence has to name: a
/// `Request` names the argument the caller got wrong, an `Engine` names the engine's own refusal.
#[derive(Debug)]
enum Failure {
    Request(String),
    Engine(hl_engine::engine::EngineError),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request(reason) => formatter.write_str(reason),
            Self::Engine(error) => write!(formatter, "the engine refused this launch: {error:?}"),
        }
    }
}

impl From<hl_engine::engine::EngineError> for Failure {
    fn from(error: hl_engine::engine::EngineError) -> Self {
        Self::Engine(error)
    }
}

impl Worker {
    pub fn run(guest: Guest) -> ! {
        let arguments = std::env::args().collect::<Vec<_>>();
        let program = Self::invoked_as(&arguments, guest);
        if arguments.get(1).map(String::as_str) == Some("--backend-receipt") {
            match backend_receipt(&arguments, Some(guest)) {
                Ok(receipt) => {
                    println!("{receipt}");
                    std::process::exit(0);
                }
                Err(reason) => {
                    eprintln!("{program}: {reason}");
                    std::process::exit(125);
                }
            }
        }
        let logging = hl_log::EnvironmentConfig::parse(hl_log::Config::default(), std::env::vars());
        for warning in logging.warnings() {
            eprintln!("{program}: {warning}");
        }
        logging.apply();

        let isa = guest.name();
        hl_log::hl_info!(hl_log::tag::EXEC, "engine process starting isa={isa}");
        hl_log::hl_event!(
            hl_log::tag::EXEC,
            hl_log::Level::Info,
            "engine.process.starting",
            isa = isa
        );
        // clap already wrote the usage message, the help page or the version line; printing its
        // error is what turns a mistyped flag from a blank screen into a diagnostic, and its own
        // exit code is what separates `--help` and `--version` (0) from a usage error (2).
        // The command is named after the worker rather than after the Cargo package, so the usage
        // line, the version line and the `tip:` suggestions all spell the binary the caller ran.
        let command = <LaunchArguments as clap::CommandFactory>::command().name(program.clone());
        let launch = command
            .try_get_matches_from(std::iter::once(program.clone()).chain(arguments.iter().skip(1).cloned()))
            .and_then(|matches| <LaunchArguments as clap::FromArgMatches>::from_arg_matches(&matches));
        let launch = match launch {
            Ok(launch) => launch,
            Err(error) => {
                let _ = error.print();
                std::process::exit(error.exit_code());
            }
        };
        let report = launch.report_exit;
        let result = execute(guest, &launch);
        if let Err(error) = &result {
            hl_log::hl_error!(hl_log::tag::EXEC, "engine process failed isa={isa} reason={error:?}");
            hl_log::hl_event!(
                hl_log::tag::EXEC,
                hl_log::Level::Error,
                "engine.process.failed",
                isa = isa,
                reason = ?error
            );
            // A worker that exits 125 without a sentence is indistinguishable from a crash, and
            // this is the only place that knows why the launch was refused.
            eprintln!("{program}: {error}");
            // Preserve the retained x86 worker's opt-in diagnostic output.
            if let Failure::Engine(engine) = error
                && guest == Guest::X86_64
                && std::env::var_os("RUST_BACKTRACE").is_some()
            {
                eprintln!("{engine:?}");
            }
        }
        let status = result.as_ref().map_or(125, |exit| exit.process_status());
        if report && let Ok(exit) = result {
            eprintln!("[hl-exit]\t{:?}\t{}\t{:#x}", exit.kind, exit.guest_status, exit.detail);
        }
        if result.is_ok() {
            hl_log::hl_info!(hl_log::tag::EXEC, "engine process exited isa={isa} status={status}");
            hl_log::hl_event!(
                hl_log::tag::EXEC,
                hl_log::Level::Info,
                "engine.process.exited",
                isa = isa,
                status = status
            );
        }
        match result {
            // A guest that died from a signal leaves this worker dead from the same signal, so the
            // launcher's wait(2) reports the crash it actually was.
            Ok(exit) => exit.exit_process(),
            Err(_) => std::process::exit(status),
        }
    }

    /// The name to put in front of a diagnostic and in clap's usage line.
    ///
    /// Argument zero carries whatever path the caller typed, so its file name is what the user
    /// will recognise; the compiled-in worker name is the fallback for an argv the host did not
    /// supply.
    fn invoked_as(arguments: &[String], guest: Guest) -> String {
        arguments
            .first()
            .map(std::path::Path::new)
            .and_then(std::path::Path::file_name)
            .and_then(std::ffi::OsStr::to_str)
            .filter(|name| !name.is_empty())
            .unwrap_or(guest.program())
            .to_owned()
    }
}

fn execute(guest: Guest, launch: &LaunchArguments) -> Result<hl_engine::engine::EngineExit, Failure> {
    if let Some(selected) = launch.guest.as_deref()
        && Guest::named(selected) != Some(guest)
    {
        return Err(Failure::Request(format!(
            "this worker runs {} guests, so it cannot serve --guest-isa {selected}",
            guest.name()
        )));
    }
    if launch.native_supervised && guest != Guest::X86_64 {
        return Err(Failure::Request(
            "--native-supervised is available only in the x86-64 worker".to_owned(),
        ));
    }
    if launch.translit_mixed_sse.is_some() && !launch.translit {
        return Err(Failure::Request("--translit-mixed-sse requires --translit".to_owned()));
    }
    if launch.translit_mixed_sse.is_some() && guest != Guest::X86_64 {
        return Err(Failure::Request(
            "--translit-mixed-sse is available only in the x86-64 worker".to_owned(),
        ));
    }
    if launch.translit_jcc_ibtc.is_some() && !launch.translit {
        return Err(Failure::Request("--translit-jcc-ibtc requires --translit".to_owned()));
    }
    if launch.translit_jcc_ibtc.is_some() && guest != Guest::X86_64 {
        return Err(Failure::Request(
            "--translit-jcc-ibtc is available only in the x86-64 worker".to_owned(),
        ));
    }
    if launch.translit_direct_jmp_ibtc.is_some() && !launch.translit {
        return Err(Failure::Request(
            "--translit-direct-jmp-ibtc requires --translit".to_owned(),
        ));
    }
    if launch.translit_direct_jmp_ibtc.is_some() && guest != Guest::X86_64 {
        return Err(Failure::Request(
            "--translit-direct-jmp-ibtc is available only in the x86-64 worker".to_owned(),
        ));
    }
    if launch.rootfs.is_none() && (launch.diagnostics || launch.translit || launch.native_supervised) {
        return Err(Failure::Request(
            "--diagnostics, --translit and --native-supervised require --rootfs; raw host-path launches do not carry launch options"
                .to_owned(),
        ));
    }
    #[cfg(feature = "native-test-hooks")]
    if launch.rootfs.is_none() && !launch.native_test_option.is_empty() {
        return Err(Failure::Request(
            "--native-test-option requires --rootfs; raw host-path launches do not carry launch options".to_owned(),
        ));
    }
    if let Some(path) = &launch.native_library {
        hl_native::select_artifact(path)
            .map_err(|reason| Failure::Request(format!("cannot select native library: {reason}")))?;
    }
    let engine = if let Some(rootfs) = &launch.rootfs {
        let plan = rootfs_plan(rootfs, launch)?;
        hl_engine::runtime::Engine::from_plan(guest.isa(), plan)?
    } else {
        let mut builder = hl_engine::runtime::Builder::new(guest.isa(), &launch.executable);
        for argument in &launch.arguments {
            builder = builder.with_argument(argument.as_bytes().to_vec());
        }
        builder.build()?
    };
    if launch.loader_receipt {
        let receipt =
            loader_receipt().map_err(|reason| Failure::Request(format!("cannot emit loader receipt: {reason}")))?;
        eprintln!("[hl-loader]\t{receipt}");
    }
    engine.start()?;
    let exit = engine.wait()?;
    engine.destroy()?;
    Ok(exit)
}

/// Builds the launch plan for a rootfs-backed run.
///
/// The entry the caller names is a *guest* path, and `RuntimePlan::executable_host` is a *host*
/// path the worker will `open()`. `rootfs.join(entry)` looks like the conversion between them and
/// is not: the host resolves any symbolic link in the joined path against the host root, so a
/// stock image's `/bin/sh -> /bin/busybox` sends the open to the host's busybox -- refused, and
/// reported only as `NativeCreateFailed(1)`. `GuestPath` is the resolver `hl-container` already
/// used for exactly this, and it re-anchors an absolute link inside the image.
fn rootfs_plan(
    rootfs: &std::path::Path,
    launch: &LaunchArguments,
) -> Result<hl_engine::launcher::plan::RuntimePlan, Failure> {
    let entry = &launch.executable;
    if entry.is_absolute()
        || entry
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(Failure::Request(format!(
            "the guest entry {} must be a plain path relative to the rootfs, with no leading slash and no `..`",
            entry.display()
        )));
    }
    if !rootfs.is_dir() {
        return Err(Failure::Request(format!(
            "the rootfs {} is not an existing directory",
            rootfs.display()
        )));
    }
    let guest_entry = std::path::Path::new("/").join(entry);
    let roots = [rootfs.to_path_buf()];
    let host = hl_engine::launcher::entry::GuestPath::host_executable(&guest_entry, &roots).ok_or_else(|| {
        Failure::Request(format!(
            "the rootfs {} has no executable file at the guest path {}",
            rootfs.display(),
            guest_entry.display()
        ))
    })?;
    let mut options = hl_engine::options::Options::default();
    for (enabled, name) in [
        (launch.diagnostics, "HL_C_DIAGNOSTICS"),
        (launch.translit, "HL_TRANSLIT"),
        (launch.native_supervised, "HL_NATIVE_SUPERVISED"),
    ] {
        if enabled {
            options
                .set(name, "1", true)
                .map_err(|error| Failure::Request(format!("cannot set the engine launch option {name}: {error:?}")))?;
        }
    }
    if launch.translit_mixed_sse == Some(MixedSseControl::Off) {
        options
            .set("HL_TRANSLIT_MIXED_SSE_DISABLE", "1", true)
            .map_err(|error| {
                Failure::Request(format!(
                    "cannot set the engine launch option HL_TRANSLIT_MIXED_SSE_DISABLE: {error:?}"
                ))
            })?;
    }
    if launch.translit_jcc_ibtc == Some(JccIbtcControl::Off) {
        options
            .set("HL_TRANSLIT_JCC_IBTC_DISABLE", "1", true)
            .map_err(|error| {
                Failure::Request(format!(
                    "cannot set the engine launch option HL_TRANSLIT_JCC_IBTC_DISABLE: {error:?}"
                ))
            })?;
    }
    // Direct-JMP late linking remains opt-in until its CLONE_VM|CLONE_VFORK
    // child-exec path has the same compatibility evidence as direct launch.
    // Store both answers explicitly: "0" makes typed ON shadow a contradictory
    // ambient disable value, while absence at the native boundary stays OFF.
    if launch.translit {
        let disabled = if launch.translit_direct_jmp_ibtc == Some(DirectJmpIbtcControl::On) {
            "0"
        } else {
            "1"
        };
        options
            .set("HL_TRANSLIT_DIRECT_JMP_IBTC_DISABLE", disabled, true)
            .map_err(|error| {
                Failure::Request(format!(
                    "cannot set the engine launch option HL_TRANSLIT_DIRECT_JMP_IBTC_DISABLE: {error:?}"
                ))
            })?;
    }
    #[cfg(feature = "native-test-hooks")]
    for injected in &launch.native_test_option {
        if options.get(injected.name).is_some() {
            return Err(Failure::Request(format!(
                "native test option {} may be specified only once",
                injected.name
            )));
        }
        options
            .set(injected.name, injected.value, false)
            .map_err(|error| Failure::Request(format!("cannot set native test option {}: {error:?}", injected.name)))?;
    }
    Ok(hl_engine::launcher::plan::RuntimePlan {
        rootfs: Some(rootfs.as_os_str().as_encoded_bytes().to_vec()),
        executable_host: Some(host.as_os_str().as_encoded_bytes().to_vec()),
        arguments: std::iter::once(guest_entry.as_os_str().as_encoded_bytes().to_vec())
            .chain(launch.arguments.iter().map(|argument| argument.as_bytes().to_vec()))
            .collect(),
        // The worker is a developer-facing raw-rootfs entry point, not an OCI launch with an image
        // configuration supplying Env.  ProductionMachine deliberately treats this vector as exact;
        // leaving it empty therefore gives the guest an actually empty environment and suppresses the
        // native loader's fallback PATH.  A shell papers over that with a non-exported local PATH, then
        // children receive no PATH: GCC cannot locate its own cc1 helper and reports posix_spawnp ENOENT.
        // Supply the small architecture-neutral baseline the retained loader historically provided.  Do
        // not inherit the host environment: that would leak developer credentials and make the guest vary
        // with the machine running it.
        environment: vec![
            b"PATH=/usr/bin:/bin".to_vec(),
            b"HOME=/root".to_vec(),
            b"LANG=C".to_vec(),
        ],
        result_path: None,
        options,
        box_policy: hl_engine::launcher::plan::RuntimeBoxPolicy {
            // Native supervision never inherits host networking. Selecting it at this developer CLI
            // boundary is an explicit request for the backend's isolated-network contract.
            flags: if launch.native_supervised { 1 << 2 } else { 0 },
            ..Default::default()
        },
    })
}

/// Emits the backend receipt, or the sentence explaining why it could not be produced.
///
/// Every arm used to answer `Err(())`, so a receipt that failed for a bad ISA name, a refused
/// engine construction and an unreadable executable were one indistinguishable exit 125.
pub fn backend_receipt(arguments: &[String], forced_guest: Option<Guest>) -> Result<String, String> {
    if arguments.get(1).map(String::as_str) != Some("--backend-receipt") {
        return Err("--backend-receipt must be the first argument".to_owned());
    }
    let parsed = BackendReceiptArguments::try_parse_from(
        std::iter::once("backend-receipt").chain(arguments[2..].iter().map(String::as_str)),
    )
    .map_err(|error| error.to_string().trim_end().replace('\n', " "))?;
    let selected = match (forced_guest, parsed.guest.as_deref()) {
        (Some(guest), Some(_)) => {
            return Err(format!(
                "this worker already fixes the guest ISA to {}, so --guest-isa cannot select another",
                guest.name()
            ));
        }
        (Some(guest), None) => Some(guest),
        (None, Some(guest)) => Some(Guest::named(guest).ok_or_else(|| format!("unknown guest ISA {guest:?}"))?),
        (None, None) => None,
    };
    let guest = selected.unwrap_or(if cfg!(target_arch = "aarch64") {
        Guest::Aarch64
    } else {
        Guest::X86_64
    });
    let plan = hl_engine::launcher::plan::RuntimePlan {
        rootfs: None,
        executable_host: None,
        arguments: vec![b"backend-receipt".to_vec()],
        environment: Vec::new(),
        result_path: None,
        options: hl_engine::options::Options::default(),
        box_policy: hl_engine::launcher::plan::RuntimeBoxPolicy::default(),
    };
    // This is the production selector itself.  A receipt is emitted only when
    // it constructs the backend named below for the requested guest ISA.
    let selected = hl_engine::runtime::Engine::from_plan(guest.isa(), plan)
        .map_err(|error| format!("the {} backend refused to construct: {error:?}", guest.name()))?;
    drop(selected);
    if !hl_native::artifact_smoke() {
        return Err("the native engine artifact failed its smoke check".to_owned());
    }

    let executable = std::env::current_exe().map_err(|error| format!("this executable has no path: {error}"))?;
    let hex = hash_path(&executable)?;
    Ok(format!(
        "{{\"schema\":\"husklet-engine-backend-v1\",\"backend\":\"retained-c\",\"engine_sha256\":\"{hex}\"}}"
    ))
}

#[cfg(unix)]
fn loader_receipt() -> Result<String, String> {
    let paths = hl_native::artifact_paths().ok_or_else(|| "the native loader exposed no path".to_owned())?;
    let mut canonical = paths
        .iter()
        .map(|path| std::fs::canonicalize(path).map_err(|error| format!("cannot resolve {}: {error}", path.display())))
        .collect::<Result<Vec<_>, _>>()?;
    canonical.sort();
    canonical.dedup();
    let [library] = canonical.as_slice() else {
        return Err(format!(
            "native lifecycle symbols resolved to {} libraries",
            canonical.len()
        ));
    };
    Ok(serde_json::json!({
        "schema": "husklet-engine-loader-v1",
        "library_sha256": hash_path(library)?,
        "library_path": library,
    })
    .to_string())
}

#[cfg(not(unix))]
fn loader_receipt() -> Result<String, String> {
    Err("loader receipts are available only on Unix".into())
}

fn hash_path(path: &std::path::Path) -> Result<String, String> {
    let mut file =
        std::fs::File::open(path).map_err(|error| format!("cannot read {} to hash it: {error}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| format!("cannot read {} to hash it: {error}", path.display()))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    let hash = digest.finalize();
    let mut hex = String::with_capacity(64);
    for byte in hash {
        use std::fmt::Write as _;
        write!(hex, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(hex)
}

#[cfg(test)]
mod tests {
    use super::{Failure, Guest, LaunchArguments, backend_receipt, execute, rootfs_plan};
    use clap::Parser;

    fn launch(arguments: &[&str]) -> LaunchArguments {
        LaunchArguments::try_parse_from(std::iter::once("hl-x86_64").chain(arguments.iter().copied())).unwrap()
    }

    fn reason(failure: &Failure) -> String {
        failure.to_string()
    }

    #[test]
    fn worker_identity_is_architecture_specific() {
        assert_eq!(Guest::Aarch64.name(), "aarch64");
        assert_eq!(Guest::Aarch64.program(), "hl-aarch64");
        assert_eq!(Guest::X86_64.name(), "x86_64");
        assert_eq!(Guest::X86_64.program(), "hl-x86_64");
    }

    #[test]
    fn backend_receipt_is_exact_and_hash_bound() {
        let receipt =
            backend_receipt(&["hl-aarch64".into(), "--backend-receipt".into()], Some(Guest::Aarch64)).unwrap();
        assert!(
            receipt.starts_with(
                "{\"schema\":\"husklet-engine-backend-v1\",\"backend\":\"retained-c\",\"engine_sha256\":\""
            )
        );
        assert!(receipt.ends_with("\"}"));
        let hash = receipt
            .strip_prefix("{\"schema\":\"husklet-engine-backend-v1\",\"backend\":\"retained-c\",\"engine_sha256\":\"")
            .unwrap()
            .strip_suffix("\"}")
            .unwrap();
        assert_eq!(hash.len(), 64);
        assert!(
            hash.bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        );
    }

    #[test]
    fn backend_receipt_rejects_an_explicit_unknown_guest() {
        assert!(
            backend_receipt(
                &[
                    "hl-engine".into(),
                    "--backend-receipt".into(),
                    "--guest-isa".into(),
                    "riscv64".into(),
                ],
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn developer_backend_flags_parse_explicitly_and_default_absent() {
        let defaults = launch(&["program"]);
        assert!(!defaults.diagnostics);
        assert!(!defaults.translit);
        assert_eq!(defaults.translit_mixed_sse, None);
        assert_eq!(defaults.translit_jcc_ibtc, None);
        assert_eq!(defaults.translit_direct_jmp_ibtc, None);
        assert!(!defaults.native_supervised);

        let selected = launch(&[
            "--diagnostics",
            "--translit",
            "--translit-mixed-sse=off",
            "--translit-jcc-ibtc=off",
            "--translit-direct-jmp-ibtc=off",
            "--native-supervised",
            "--rootfs",
            "/image",
            "bin/program",
        ]);
        assert!(selected.diagnostics);
        assert!(selected.translit);
        assert_eq!(selected.translit_mixed_sse, Some(super::MixedSseControl::Off));
        assert_eq!(selected.translit_jcc_ibtc, Some(super::JccIbtcControl::Off));
        assert_eq!(
            selected.translit_direct_jmp_ibtc,
            Some(super::DirectJmpIbtcControl::Off)
        );
        assert!(selected.native_supervised);
        assert_eq!(selected.rootfs.as_deref(), Some(std::path::Path::new("/image")));
    }

    #[test]
    fn mixed_sse_control_is_typed_and_requires_transliteration() {
        for invalid in ["yes", "0", "disabled", ""] {
            let option = format!("--translit-mixed-sse={invalid}");
            assert!(
                LaunchArguments::try_parse_from(["hl-x86_64", option.as_str(), "program",]).is_err(),
                "accepted {invalid:?}"
            );
        }
        let failure = execute(
            Guest::X86_64,
            &launch(&["--translit-mixed-sse=off", "--rootfs", "/image", "bin/program"]),
        )
        .unwrap_err();
        assert!(reason(&failure).contains("--translit-mixed-sse requires --translit"));

        let failure = execute(
            Guest::Aarch64,
            &launch(&[
                "--translit",
                "--translit-mixed-sse=off",
                "--rootfs",
                "/image",
                "bin/program",
            ]),
        )
        .unwrap_err();
        assert!(reason(&failure).contains("available only in the x86-64 worker"));
    }

    #[test]
    fn jcc_ibtc_control_is_typed_and_x86_transliteration_only() {
        for invalid in ["yes", "0", "disabled", ""] {
            let option = format!("--translit-jcc-ibtc={invalid}");
            assert!(
                LaunchArguments::try_parse_from(["hl-x86_64", option.as_str(), "program"]).is_err(),
                "accepted {invalid:?}"
            );
        }
        let failure = execute(
            Guest::X86_64,
            &launch(&["--translit-jcc-ibtc=off", "--rootfs", "/image", "bin/program"]),
        )
        .unwrap_err();
        assert!(reason(&failure).contains("--translit-jcc-ibtc requires --translit"));

        let failure = execute(
            Guest::Aarch64,
            &launch(&[
                "--translit",
                "--translit-jcc-ibtc=off",
                "--rootfs",
                "/image",
                "bin/program",
            ]),
        )
        .unwrap_err();
        assert!(reason(&failure).contains("available only in the x86-64 worker"));
    }

    #[test]
    fn direct_jmp_ibtc_control_is_typed_and_x86_transliteration_only() {
        for invalid in ["yes", "0", "disabled", ""] {
            let option = format!("--translit-direct-jmp-ibtc={invalid}");
            assert!(
                LaunchArguments::try_parse_from(["hl-x86_64", option.as_str(), "program"]).is_err(),
                "accepted {invalid:?}"
            );
        }
        let failure = execute(
            Guest::X86_64,
            &launch(&["--translit-direct-jmp-ibtc=off", "--rootfs", "/image", "bin/program"]),
        )
        .unwrap_err();
        assert!(reason(&failure).contains("--translit-direct-jmp-ibtc requires --translit"));
        let failure = execute(
            Guest::Aarch64,
            &launch(&[
                "--translit",
                "--translit-direct-jmp-ibtc=off",
                "--rootfs",
                "/image",
                "bin/program",
            ]),
        )
        .unwrap_err();
        assert!(reason(&failure).contains("available only in the x86-64 worker"));
    }

    #[cfg(not(feature = "native-test-hooks"))]
    #[test]
    fn production_parser_rejects_native_test_options() {
        let parsed = LaunchArguments::try_parse_from([
            "hl-x86_64",
            "--native-test-option",
            "HL_TRANSLIT_FS_AUTHORITY_TEST=1",
            "program",
        ]);
        assert_eq!(parsed.unwrap_err().kind(), clap::error::ErrorKind::UnknownArgument);
    }

    #[cfg(feature = "native-test-hooks")]
    #[test]
    fn hooks_parser_accepts_only_the_typed_fs_authority_injection() {
        let selected = launch(&[
            "--native-test-option",
            "HL_TRANSLIT_FS_AUTHORITY_TEST=1",
            "--rootfs",
            "/image",
            "bin/program",
        ]);
        assert_eq!(
            selected.native_test_option,
            [super::NativeTestOption {
                name: "HL_TRANSLIT_FS_AUTHORITY_TEST",
                value: "1"
            }]
        );

        for invalid in [
            "HL_TRANSLIT_FS_AUTHORITY_TEST",
            "HL_TRANSLIT_FS_AUTHORITY_TEST=0",
            "HL_C_DIAGNOSTICS=1",
        ] {
            assert!(
                LaunchArguments::try_parse_from([
                    "hl-x86_64",
                    "--native-test-option",
                    invalid,
                    "--rootfs",
                    "/image",
                    "bin/program",
                ])
                .is_err(),
                "accepted {invalid}"
            );
        }
    }

    #[cfg(feature = "native-test-hooks")]
    #[test]
    fn native_test_options_require_a_rootfs_instead_of_being_ignored() {
        let failure = execute(
            Guest::X86_64,
            &launch(&["--native-test-option", "HL_TRANSLIT_FS_AUTHORITY_TEST=1", "/bin/true"]),
        )
        .unwrap_err();
        assert!(reason(&failure).contains("--native-test-option requires --rootfs"));
    }

    #[test]
    fn launch_options_are_rejected_for_a_host_path_instead_of_ignored() {
        for option in ["--diagnostics", "--translit", "--native-supervised"] {
            let failure = execute(Guest::X86_64, &launch(&[option, "/bin/true"])).unwrap_err();
            assert!(reason(&failure).contains("require --rootfs"));
        }
        let failure = execute(
            Guest::X86_64,
            &launch(&["--translit", "--translit-jcc-ibtc=off", "/bin/true"]),
        )
        .unwrap_err();
        assert!(reason(&failure).contains("require --rootfs"));
    }

    #[cfg(unix)]
    #[test]
    fn rootfs_plan_carries_only_the_explicit_backend_options() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = tempfile::tempdir().unwrap();
        let program = root.path().join("bin/program");
        std::fs::create_dir_all(program.parent().unwrap()).unwrap();
        std::fs::write(&program, b"\x7fELF").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();

        let defaults = rootfs_plan(
            root.path(),
            &launch(&["--rootfs", root.path().to_str().unwrap(), "bin/program"]),
        )
        .unwrap();
        assert_eq!(defaults.options.get("HL_C_DIAGNOSTICS"), None);
        assert_eq!(defaults.options.get("HL_TRANSLIT"), None);
        assert_eq!(defaults.options.get("HL_TRANSLIT_MIXED_SSE_DISABLE"), None);
        assert_eq!(defaults.options.get("HL_TRANSLIT_JCC_IBTC_DISABLE"), None);
        assert_eq!(defaults.options.get("HL_TRANSLIT_DIRECT_JMP_IBTC_DISABLE"), None);
        assert_eq!(defaults.options.get("HL_NATIVE_SUPERVISED"), None);

        let selected = rootfs_plan(
            root.path(),
            &launch(&[
                "--diagnostics",
                "--translit",
                "--translit-mixed-sse=off",
                "--translit-jcc-ibtc=off",
                "--translit-direct-jmp-ibtc=off",
                "--native-supervised",
                "--rootfs",
                root.path().to_str().unwrap(),
                "bin/program",
            ]),
        )
        .unwrap();
        assert_eq!(selected.options.get("HL_C_DIAGNOSTICS"), Some("1"));
        assert_eq!(selected.options.get("HL_TRANSLIT"), Some("1"));
        assert_eq!(selected.options.get("HL_TRANSLIT_MIXED_SSE_DISABLE"), Some("1"));
        assert_eq!(selected.options.get("HL_TRANSLIT_JCC_IBTC_DISABLE"), Some("1"));
        assert_eq!(selected.options.get("HL_TRANSLIT_DIRECT_JMP_IBTC_DISABLE"), Some("1"));
        assert!(
            selected
                .environment
                .iter()
                .all(|entry| !entry.starts_with(b"HL_TRANSLIT_MIXED_SSE_DISABLE=")),
            "launch options must not enter the guest environment"
        );
        assert!(
            selected
                .environment
                .iter()
                .all(|entry| !entry.starts_with(b"HL_TRANSLIT_DIRECT_JMP_IBTC_DISABLE=")),
            "direct-JMP IBTC launch policy must not enter the guest environment"
        );
        assert!(
            selected
                .environment
                .iter()
                .all(|entry| !entry.starts_with(b"HL_TRANSLIT_JCC_IBTC_DISABLE=")),
            "JCC IBTC launch policy must not enter the guest environment"
        );
        assert_eq!(selected.options.get("HL_NATIVE_SUPERVISED"), Some("1"));
        assert_eq!(selected.box_policy.flags & (1 << 2), 1 << 2);
        assert_eq!(defaults.box_policy.flags & (1 << 2), 0);

        let explicitly_enabled = rootfs_plan(
            root.path(),
            &launch(&[
                "--translit",
                "--translit-mixed-sse=on",
                "--translit-jcc-ibtc=on",
                "--translit-direct-jmp-ibtc=on",
                "--rootfs",
                root.path().to_str().unwrap(),
                "bin/program",
            ]),
        )
        .unwrap();
        assert_eq!(explicitly_enabled.options.get("HL_TRANSLIT_MIXED_SSE_DISABLE"), None);
        assert_eq!(explicitly_enabled.options.get("HL_TRANSLIT_JCC_IBTC_DISABLE"), None);
        assert_eq!(
            explicitly_enabled.options.get("HL_TRANSLIT_DIRECT_JMP_IBTC_DISABLE"),
            Some("0")
        );

        let translit_default = rootfs_plan(
            root.path(),
            &launch(&["--translit", "--rootfs", root.path().to_str().unwrap(), "bin/program"]),
        )
        .unwrap();
        assert_eq!(
            translit_default.options.get("HL_TRANSLIT_DIRECT_JMP_IBTC_DISABLE"),
            Some("1")
        );
    }

    #[cfg(all(unix, feature = "native-test-hooks"))]
    #[test]
    fn rootfs_plan_binds_one_native_test_option_and_rejects_a_duplicate() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = tempfile::tempdir().unwrap();
        let program = root.path().join("bin/program");
        std::fs::create_dir_all(program.parent().unwrap()).unwrap();
        std::fs::write(&program, b"\x7fELF").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let root = root.path().to_str().unwrap();

        let plan = rootfs_plan(
            std::path::Path::new(root),
            &launch(&[
                "--native-test-option",
                "HL_TRANSLIT_FS_AUTHORITY_TEST=1",
                "--rootfs",
                root,
                "bin/program",
            ]),
        )
        .unwrap();
        assert_eq!(plan.options.get("HL_TRANSLIT_FS_AUTHORITY_TEST"), Some("1"));

        let duplicate = rootfs_plan(
            std::path::Path::new(root),
            &launch(&[
                "--native-test-option",
                "HL_TRANSLIT_FS_AUTHORITY_TEST=1",
                "--native-test-option",
                "HL_TRANSLIT_FS_AUTHORITY_TEST=1",
                "--rootfs",
                root,
                "bin/program",
            ]),
        )
        .unwrap_err();
        assert_eq!(
            reason(&duplicate),
            "native test option HL_TRANSLIT_FS_AUTHORITY_TEST may be specified only once"
        );
    }

    /// A stock image ships `/bin/sh` as an **absolute** symbolic link, and the host path the plan
    /// carries is what the worker `open()`s. Resolving that link against the host root -- which a
    /// plain `rootfs.join(entry)` does -- means the guest cannot run its own shell, and the only
    /// thing the user sees is `NativeCreateFailed(1)` behind `RUST_BACKTRACE`.
    #[cfg(unix)]
    #[test]
    fn an_absolute_image_symlink_resolves_inside_the_rootfs() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let root = tempfile::tempdir().unwrap();
        let busybox = root.path().join("bin/busybox");
        std::fs::create_dir_all(busybox.parent().unwrap()).unwrap();
        std::fs::write(&busybox, b"\x7fELF").unwrap();
        std::fs::set_permissions(&busybox, std::fs::Permissions::from_mode(0o755)).unwrap();
        symlink("/bin/busybox", root.path().join("bin/sh")).unwrap();

        let plan = rootfs_plan(
            root.path(),
            &launch(&["--rootfs", root.path().to_str().unwrap(), "bin/sh", "-c", "echo"]),
        )
        .unwrap();
        assert_eq!(
            plan.executable_host.as_deref(),
            Some(busybox.as_os_str().as_encoded_bytes())
        );
        assert_eq!(plan.arguments[0], b"/bin/sh");
        assert_eq!(plan.arguments[1], b"-c");
    }

    /// The relative spelling of the same link, which already worked, must keep working.
    #[cfg(unix)]
    #[test]
    fn a_relative_image_symlink_still_resolves_inside_the_rootfs() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let root = tempfile::tempdir().unwrap();
        let busybox = root.path().join("bin/busybox");
        std::fs::create_dir_all(busybox.parent().unwrap()).unwrap();
        std::fs::write(&busybox, b"\x7fELF").unwrap();
        std::fs::set_permissions(&busybox, std::fs::Permissions::from_mode(0o755)).unwrap();
        symlink("busybox", root.path().join("bin/sh")).unwrap();

        let plan = rootfs_plan(
            root.path(),
            &launch(&["--rootfs", root.path().to_str().unwrap(), "bin/sh"]),
        )
        .unwrap();
        assert_eq!(
            plan.executable_host.as_deref(),
            Some(busybox.as_os_str().as_encoded_bytes())
        );
    }

    /// A raw-rootfs worker has no OCI image configuration from which to obtain `Env`.  The runtime
    /// treats the plan's environment as exact, so an empty vector is not a request for its C loader's
    /// defaults: it is an authoritative empty environment.  Alpine ash masks that at its prompt with a
    /// shell-local PATH, but does not export the invented value.  GCC then starts with no PATH and cannot
    /// derive the absolute location of `cc1` from its `cc` argv[0].
    #[cfg(unix)]
    #[test]
    fn a_rootfs_worker_exports_a_bounded_guest_environment() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = tempfile::tempdir().unwrap();
        let program = root.path().join("bin/program");
        std::fs::create_dir_all(program.parent().unwrap()).unwrap();
        std::fs::write(&program, b"\x7fELF").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();

        let plan = rootfs_plan(
            root.path(),
            &launch(&["--rootfs", root.path().to_str().unwrap(), "bin/program"]),
        )
        .unwrap();

        assert_eq!(
            plan.environment,
            [
                b"PATH=/usr/bin:/bin".as_slice(),
                b"HOME=/root".as_slice(),
                b"LANG=C".as_slice()
            ]
        );
        assert!(
            plan.environment
                .iter()
                .all(|record| !record.starts_with(b"NIX_") && !record.starts_with(b"AWS_")),
            "the worker must not replace a missing guest environment by copying host credentials"
        );
    }

    /// A developer who mistypes any of these gets a sentence naming what was wrong, not a bare
    /// exit code. Each string below is what reaches descriptor 2.
    #[test]
    fn every_refused_launch_names_what_was_wrong() {
        let root = tempfile::tempdir().unwrap();
        let present = root.path().to_str().unwrap().to_owned();

        assert_eq!(
            reason(
                &rootfs_plan(
                    std::path::Path::new("/no/such/rootfs"),
                    &launch(&["--rootfs", "/no/such/rootfs", "bin/sh"])
                )
                .unwrap_err()
            ),
            "the rootfs /no/such/rootfs is not an existing directory"
        );
        assert_eq!(
            reason(&rootfs_plan(root.path(), &launch(&["--rootfs", &present, "bin/nope"])).unwrap_err()),
            format!("the rootfs {present} has no executable file at the guest path /bin/nope")
        );
        assert_eq!(
            reason(&rootfs_plan(root.path(), &launch(&["--rootfs", &present, "../escape"])).unwrap_err()),
            "the guest entry ../escape must be a plain path relative to the rootfs, with no leading slash and no `..`"
        );
        assert_eq!(
            reason(&execute(Guest::X86_64, &launch(&["--guest-isa", "aarch64", "bin/sh"])).unwrap_err()),
            "this worker runs x86_64 guests, so it cannot serve --guest-isa aarch64"
        );
    }

    /// `--help` and `--version` are answers, not failures: clap's own exit code is 0 for both and
    /// 2 for a usage error, which is exactly the distinction the worker used to throw away.
    #[test]
    fn help_and_version_are_answered_and_a_bad_flag_is_explained() {
        let help = LaunchArguments::try_parse_from(["hl-x86_64", "--help"]).unwrap_err();
        assert_eq!(help.exit_code(), 0);
        assert!(help.to_string().contains("--rootfs"), "{help}");

        let version = LaunchArguments::try_parse_from(["hl-x86_64", "--version"]).unwrap_err();
        assert_eq!(version.exit_code(), 0);

        let unknown = LaunchArguments::try_parse_from(["hl-x86_64", "--rootfsx", "/tmp", "bin/sh"]).unwrap_err();
        assert_eq!(unknown.exit_code(), 2);
        assert!(unknown.to_string().contains("--rootfsx"), "{unknown}");

        let missing = LaunchArguments::try_parse_from(["hl-x86_64"]).unwrap_err();
        assert_eq!(missing.exit_code(), 2);
        assert!(missing.to_string().contains("EXECUTABLE"), "{missing}");
    }

    #[test]
    fn a_refused_receipt_says_why() {
        assert_eq!(
            backend_receipt(&["hl-engine".into(), "--guest-isa".into()], None).unwrap_err(),
            "--backend-receipt must be the first argument"
        );
        assert_eq!(
            backend_receipt(
                &[
                    "hl-engine".into(),
                    "--backend-receipt".into(),
                    "--guest-isa".into(),
                    "riscv64".into(),
                ],
                None,
            )
            .unwrap_err(),
            "unknown guest ISA \"riscv64\""
        );
        assert_eq!(
            backend_receipt(
                &[
                    "hl-x86_64".into(),
                    "--backend-receipt".into(),
                    "--guest-isa".into(),
                    "x86_64".into(),
                ],
                Some(Guest::X86_64),
            )
            .unwrap_err(),
            "this worker already fixes the guest ISA to x86_64, so --guest-isa cannot select another"
        );
    }

    #[test]
    fn a_diagnostic_is_prefixed_with_the_name_the_caller_typed() {
        assert_eq!(
            super::Worker::invoked_as(&["/srv/build/target/release/hl-x86_64".to_owned()], Guest::Aarch64),
            "hl-x86_64"
        );
        assert_eq!(super::Worker::invoked_as(&[], Guest::Aarch64), "hl-aarch64");
    }

    #[test]
    fn launch_parser_owns_rootfs_and_trailing_guest_arguments() {
        let launch = LaunchArguments::try_parse_from([
            "hl-x86_64",
            "--rootfs",
            "/staged/rootfs",
            "usr/local/bin/python3",
            "-c",
            "print(42)",
        ])
        .unwrap();
        assert_eq!(launch.rootfs.unwrap(), std::path::Path::new("/staged/rootfs"));
        assert_eq!(launch.executable, std::path::Path::new("usr/local/bin/python3"));
        assert_eq!(launch.arguments, ["-c", "print(42)"]);
    }
}
