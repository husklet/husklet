use std::{fs, path::PathBuf, time::SystemTime};

use crate::rule::Rule;

use super::EnvironmentAccess;

fn findings_in(package_name: &str, source: &str, relative: &str) -> Vec<crate::Finding> {
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("clock follows Unix epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("hl-env-rule-{nonce}"));
    let package = root.join(format!("src/packages/{package_name}"));
    let path = package.join(relative);
    fs::create_dir_all(path.parent().expect("fixture has a parent")).expect("create fixture");
    fs::write(
        package.join("Cargo.toml"),
        format!("[package]\nname = \"{package_name}\"\nversion = \"0.0.0\"\n"),
    )
    .expect("write manifest");
    fs::write(&path, source).expect("write fixture");
    let workspace = crate::source::Workspace::load([PathBuf::from(&path)]).expect("parse fixture");
    let values = EnvironmentAccess.check(&workspace).expect("run rule");
    fs::remove_dir_all(root).expect("remove fixture");
    values
}

fn findings(source: &str, relative: &str) -> Vec<crate::Finding> {
    findings_in("fixture", source, relative)
}

#[test]
fn resolves_std_env_aliases_and_host_path_apis() {
    let values = findings(
        r#"
use std::env::{self as process_environment, current_dir as cwd, var as read};
use std::env as host;
fn load() {
    let _ = read("A");
    let _ = process_environment::vars_os();
    let _ = host::current_exe();
    let _ = cwd();
    let _ = std::env::temp_dir();
}
"#,
        "src/lib.rs",
    );
    assert_eq!(values.len(), 5);
    assert!(values.iter().all(crate::Finding::is_violation));
}

#[test]
fn resolves_renamed_dirs_crate_without_matching_similar_names() {
    let values = findings(
        r#"
use dirs as locations;
use dirs::config_dir as preferences;
fn load() {
    let _ = locations::home_dir();
    let _ = preferences();
    let _ = my_dirs::home_dir();
}
"#,
        "src/lib.rs",
    );
    assert_eq!(values.len(), 2);
}

#[test]
fn permits_tests_build_scripts_and_explicit_adapters() {
    assert!(findings(
        "fn load() { let _ = std::env::var(\"A\"); }",
        "src/adapter/host.rs"
    )
    .is_empty());
    assert!(findings("fn main() { let _ = std::env::var(\"A\"); }", "build.rs").is_empty());
    assert!(findings(
        "#[test] fn load() { let _ = std::env::var(\"A\"); }",
        "src/lib.rs"
    )
    .is_empty());
}

#[test]
fn role_words_outside_an_adapter_boundary_do_not_suppress_findings() {
    for relative in ["src/domain/host.rs", "src/model/linux.rs"] {
        let values = findings("fn load() { let _ = std::env::current_dir(); }", relative);
        assert_eq!(values.len(), 1, "{relative} must not be an adapter");
    }
    let values = findings_in(
        "ordinary-wgpu",
        "fn load() { let _ = std::env::var(\"A\"); }",
        "src/device.rs",
    );
    assert_eq!(values.len(), 1);
}

#[test]
fn structurally_explicit_platform_boundaries_are_permitted() {
    for relative in [
        "src/adapter/wayland.rs",
        "src/adapters/macos.rs",
        "src/platform/linux.rs",
        "src/host.rs",
    ] {
        assert!(
            findings("fn load() { let _ = std::env::current_exe(); }", relative,).is_empty(),
            "{relative} is an explicit platform boundary"
        );
    }
}

#[test]
fn configuration_globals_require_semantic_evidence() {
    let values = findings(
        r#"
use std::sync::{Mutex, OnceLock};
struct AppConfig;
struct State;
static CONFIG: OnceLock<AppConfig> = OnceLock::new();
static STATE: OnceLock<Mutex<State>> = OnceLock::new();
static LOCKS: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
static REGISTRY: OnceLock<Vec<String>> = OnceLock::new();
"#,
        "src/lib.rs",
    );
    assert_eq!(values.len(), 2);
    assert!(values.iter().any(|value| value.subject == "CONFIG"));
    assert!(values.iter().any(|value| value.subject == "STATE"));
}
