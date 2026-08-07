//! Records the cargo profile so a run can report which engine build produced it.

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "unknown".to_owned());
    println!("cargo::rustc-env=HL_TESTING_PROFILE={profile}");
}
