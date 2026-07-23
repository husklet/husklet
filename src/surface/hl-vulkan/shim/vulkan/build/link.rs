use std::path::{Path, PathBuf};

pub struct Directives {
    manifest: PathBuf,
}

impl Directives {
    pub fn new(manifest: &Path) -> Self {
        Self {
            manifest: manifest.to_owned(),
        }
    }

    pub fn emit(&self) {
        println!("cargo:rerun-if-changed={}", self.manifest.display());
        println!("cargo:rerun-if-changed=build.rs");

        // The ICD ships as a Linux guest library; GNU soname syntax is invalid on host platforms.
        if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
            println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,libvk_hl.so.1");
        }
    }
}
