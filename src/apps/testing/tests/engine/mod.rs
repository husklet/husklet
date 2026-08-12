use std::path::PathBuf;

pub struct EngineBinaryPaths {
    root: PathBuf,
}

impl EngineBinaryPaths {
    pub fn required() -> Self {
        let root = std::env::var_os("HL_TEST_ENGINE_APP_BIN_DIR").unwrap_or_else(|| {
            panic!("HL_TEST_ENGINE_APP_BIN_DIR must name the absolute directory containing engine app binaries")
        });
        let root = PathBuf::from(root);
        assert!(root.is_absolute(), "HL_TEST_ENGINE_APP_BIN_DIR must be absolute");
        Self { root }
    }

    pub fn named(&self, name: &str) -> PathBuf {
        let path = self.root.join(name);
        assert!(
            path.is_file(),
            "required engine app binary is missing: {}",
            path.display()
        );
        path
    }
}
