//! Deterministic native translation-unit discovery.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use crate::build_support;

pub const NATIVE_ROOT: &str = "src/native";

const EXPLICIT_ROOTS: &[&str] = &[
    "src/native/bridge/shim.c",
    "src/native/bridge/host.c",
    "src/native/bridge/executable_authority.c",
    "src/native/bridge/address_projection.c",
    "src/native/engine/lifecycle.c",
    "src/native/engine/target/aarch64.c",
    "src/native/engine/target/x86_64.c",
    "src/native/engine/checkpoint_channel.c",
];

/* Format slices under independent review are compiled by their direct native
 * probes, not shipped in the runtime archive before production integration. */
const DEFERRED_ROOTS: &[&str] = &["src/native/engine/arena_sidecar.c"];

pub fn emit_rerun_inputs(directory: &Path) {
    println!("cargo:rerun-if-changed={}", directory.display());
    for path in sorted_entries(directory) {
        if path.is_dir() {
            emit_rerun_inputs(&path);
        } else if path.is_file() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}

pub fn runtime_roots(target_os: &str, target_arch: &str, package: &Path) -> Vec<String> {
    let mut sources = BTreeSet::new();
    collect_c_sources(Path::new(NATIVE_ROOT), &mut sources);
    let included = sources
        .iter()
        .flat_map(|source| included_c_sources(source, package))
        .collect::<BTreeSet<_>>();
    sources
        .into_iter()
        .filter(|source| !included.contains(source))
        .filter(|source| !EXPLICIT_ROOTS.contains(&source.to_string_lossy().as_ref()))
        .filter(|source| !DEFERRED_ROOTS.contains(&source.to_string_lossy().as_ref()))
        .filter(|source| build_support::source_matches(target_os, &source.to_string_lossy()))
        .filter(|source| {
            target_arch == "aarch64" || !source.to_string_lossy().contains("/translator/guest/x86_64/lower/")
        })
        .map(|source| source.to_string_lossy().into_owned())
        .collect()
}

fn collect_c_sources(directory: &Path, output: &mut BTreeSet<PathBuf>) {
    for path in sorted_entries(directory) {
        if path.is_dir() {
            collect_c_sources(&path, output);
        } else if path.extension().and_then(|value| value.to_str()) == Some("c") {
            output.insert(path);
        }
    }
}

fn included_c_sources(source: &Path, package: &Path) -> Vec<PathBuf> {
    let text = fs::read_to_string(source).unwrap_or_else(|error| panic!("read {}: {error}", source.display()));
    text.lines()
        .filter_map(|line| {
            let include = line.trim_start().strip_prefix("#include \"")?.split('"').next()?;
            Path::new(include)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("c"))
                .then(|| {
                    source
                        .parent()
                        .expect("source parent")
                        .join(include)
                        .canonicalize()
                        .unwrap_or_else(|error| {
                            panic!("resolve C include {include} from {}: {error}", source.display())
                        })
                })
        })
        .map(|path| {
            let package = package.canonicalize().expect("canonical package");
            path.strip_prefix(package).expect("package-local C include").to_owned()
        })
        .collect()
}

fn sorted_entries(directory: &Path) -> Vec<PathBuf> {
    let mut entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|error| panic!("enumerate {}: {error}", directory.display()));
    entries.sort_by_key(fs::DirEntry::file_name);
    entries.into_iter().map(|entry| entry.path()).collect()
}
