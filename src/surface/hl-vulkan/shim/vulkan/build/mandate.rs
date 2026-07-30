//! `registry/vk_core_mandate.manifest` — the core Vulkan version that first mandates each command.
//!
//! Read from the Khronos registry, not from this driver's own source, so the set of commands a version
//! obliges the driver to implement cannot drift with the implementation.

use std::path::PathBuf;

pub struct Mandate {
    entries: Vec<(u32, String)>,
}

impl Mandate {
    pub fn load() -> Self {
        let path = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
            .join("registry/vk_core_mandate.manifest");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let mut entries = Vec::new();
        for (number, line) in text.lines().enumerate() {
            let line = line.trim_end();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut fields = line.split('\t');
            if fields.next() != Some("M") {
                panic!("vk_core_mandate.manifest line {}: {line:?}", number + 1);
            }
            let version = fields.next().unwrap_or("");
            let name = fields.next().unwrap_or("");
            entries.push((Self::packed(version, number + 1), name.to_string()));
        }
        println!("cargo:rerun-if-changed={}", path.display());
        Self { entries }
    }

    pub fn entries(&self) -> &[(u32, String)] {
        &self.entries
    }

    /// `major.minor` -> `VK_MAKE_API_VERSION(0, major, minor, 0)`, the packing `HL_API_VERSION` uses.
    fn packed(version: &str, line: usize) -> u32 {
        let (major, minor) = version
            .split_once('.')
            .unwrap_or_else(|| panic!("vk_core_mandate.manifest line {line}: version {version:?}"));
        let parse = |value: &str| {
            value
                .parse::<u32>()
                .unwrap_or_else(|_| panic!("vk_core_mandate.manifest line {line}: {version:?}"))
        };
        (parse(major) << 22) | (parse(minor) << 12)
    }
}
