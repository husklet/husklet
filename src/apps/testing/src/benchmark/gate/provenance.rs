use super::{Isa, Path, PathBuf};

pub(crate) struct Provenance {
    pub(super) build_id: String,
    pub(super) revision: String,
    pub(super) rust_sha256: String,
    pub(super) host_load: String,
}

impl Provenance {
    pub(super) fn dirty(&self) -> bool {
        self.revision.ends_with("-dirty")
    }

    pub(super) fn print(&self, workload: &str, cpu: usize, repeats: usize) {
        println!(
            "provenance\tworkload={workload}\trust_revision={}\trust_sha256={}\tc_build_id={}\tcpu={cpu}\trepeats={repeats}\thost_load={}",
            self.revision, self.rust_sha256, self.build_id, self.host_load
        );
    }
}

/// The `hl-engine` binary is produced by the `engine` package, so `cargo build -p hl-engine`
/// builds only the library and leaves whatever binary a previous or foreign build left behind.
pub(crate) const ENGINE_BUILD: [&str; 7] = ["build", "--release", "--locked", "-p", "engine", "--bin", "hl-engine"];

/// Builds the engine binary and reports where cargo put it, so the gate measures this tree's
/// binary instead of trusting a path another lane may have written.
pub(super) fn build_engine() -> Result<PathBuf, String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = crate::platform::HostProcess::standard(cargo)
        .args(ENGINE_BUILD)
        .arg("--message-format=json-render-diagnostics")
        .output()
        .map_err(|error| format!("build the engine binary: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo {} failed:\n{}",
            ENGINE_BUILD.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    artifact(&String::from_utf8_lossy(&output.stdout))
        .ok_or_else(|| format!("cargo {} produced no hl-engine executable", ENGINE_BUILD.join(" ")))
}

/// Extracts the `hl-engine` executable path from a cargo JSON artifact stream.
pub(super) fn artifact(stream: &str) -> Option<PathBuf> {
    stream
        .lines()
        .filter_map(|line| line.split("\"executable\":\"").nth(1))
        .filter_map(|tail| tail.split('"').next())
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .find(|path| path.file_name().is_some_and(|name| name == "hl-engine"))
}

/// The source revision the Rust engine under test was built from.
pub(crate) fn revision() -> String {
    let git = |arguments: &[&str]| {
        crate::platform::HostProcess::standard("git")
            .args(arguments)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    let Some(head) = git(&["rev-parse", "--short=12", "HEAD"]).filter(|head| !head.is_empty()) else {
        return "unknown".into();
    };
    match git(&["status", "--porcelain"]) {
        Some(status) if status.is_empty() => head,
        _ => format!("{head}-dirty"),
    }
}

pub(super) fn ratio(value: u64, reference: u64) -> f64 {
    if reference == 0 {
        f64::INFINITY
    } else {
        value as f64 / reference as f64
    }
}

/// Parses `Cpus_allowed_list` and reports the CPU the run must be pinned to.
pub(crate) fn pinning(affinity: &str, requested: Option<usize>) -> Result<(usize, bool), String> {
    slot(affinity, requested, std::process::id() as usize)
}

/// Spreads defaulted pins across the allowed set by `seed`, so concurrent lanes
/// do not all land on the same core.
pub(super) fn slot(affinity: &str, requested: Option<usize>, seed: usize) -> Result<(usize, bool), String> {
    let mut allowed = Vec::new();
    for span in affinity.trim().split(',') {
        let (start, end) = span.split_once('-').unwrap_or((span, span));
        let start = start.trim().parse::<usize>().map_err(|_| "unreadable CPU affinity")?;
        let end = end.trim().parse::<usize>().map_err(|_| "unreadable CPU affinity")?;
        if end < start {
            return Err("unreadable CPU affinity".into());
        }
        allowed.extend(start..=end);
    }
    if allowed.is_empty() {
        return Err("unreadable CPU affinity".into());
    }
    // CPU 0 carries most IRQ work, so defaults avoid it whenever anything else is allowed.
    let pool = if allowed.len() > 1 && allowed[0] == 0 {
        &allowed[1..]
    } else {
        &allowed[..]
    };
    let cpu = requested.unwrap_or_else(|| pool[seed % pool.len()]);
    if !allowed.contains(&cpu) {
        return Err(format!("CPU {cpu} is outside the inherited affinity {affinity}"));
    }
    Ok((cpu, allowed.as_slice() == [cpu]))
}

impl Isa {
    /// The guest lowering a run actually exercises, so nobody proves an
    /// x86-64 change by measuring an ARM64 guest.
    pub(crate) const fn lowering(self) -> &'static str {
        match self {
            Self::Aarch64 => "src/runtime/native/exec/src/arch/aarch64",
            Self::X86 => "src/runtime/native/exec/src/arch/x86_64",
        }
    }
}

/// Names every prerequisite a lane must build before the gate can measure.
pub(crate) fn missing(guest: &Path, rust_engine: &Path, c_engine: &Path, runner: &Path, arch: &str) -> Vec<String> {
    let mut missing = Vec::new();
    if !guest.is_file() {
        missing.push(format!(
            "guest {} is missing: make bench-guest BENCH_ARCH={arch}",
            guest.display()
        ));
    }
    if !rust_engine.is_file() {
        missing.push(format!(
            "rust engine {} is missing: cargo {}",
            rust_engine.display(),
            ENGINE_BUILD.join(" ")
        ));
    }
    if !c_engine.is_file() {
        missing.push(format!(
            "retained C engine {} is missing: build it in the engine tree and pass --c-build",
            c_engine.display()
        ));
    }
    if !runner.is_file() {
        missing.push(format!("retained C exec wrapper {} is missing", runner.display()));
    }
    missing
}

/// Resolves the retained engine and its exec wrapper from a build root.
pub(crate) fn wiring(root: &Path, isa: Isa) -> (PathBuf, PathBuf) {
    (
        root.join("linux-production")
            .join(format!("hl-engine-linux-{}", isa.name())),
        root.join("bin").join("hl-engine-runner"),
    )
}
