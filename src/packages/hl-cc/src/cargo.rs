use std::path::Path;

pub struct CargoDirectives;

impl CargoDirectives {
    pub fn rerun_file(path: impl AsRef<Path>) {
        println!("cargo:rerun-if-changed={}", path.as_ref().display());
    }
    pub fn rerun_environment(name: &str) {
        println!("cargo:rerun-if-env-changed={name}");
    }
    pub fn cfg(name: &str, value: impl std::fmt::Display) {
        println!("cargo:{name}={value}");
    }
    pub fn rustc_environment(name: &str, value: impl std::fmt::Display) {
        println!("cargo:rustc-env={name}={value}");
    }
    pub fn warning(message: impl std::fmt::Display) {
        println!("cargo:warning={message}");
    }
    pub fn link_search(path: impl AsRef<Path>) {
        println!("cargo:rustc-link-search=native={}", path.as_ref().display());
    }
    pub fn link_library(kind: Option<&str>, name: &str) {
        match kind {
            Some(kind) => println!("cargo:rustc-link-lib={kind}={name}"),
            None => println!("cargo:rustc-link-lib={name}"),
        }
    }
    pub fn link_argument(argument: impl std::fmt::Display) {
        println!("cargo:rustc-link-arg={argument}");
    }
}
