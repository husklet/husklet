use std::path::{Path, PathBuf};

pub(super) const SOURCE: &str = "tests/benchmark/three-arm/malloc.c";

pub(super) struct Layout {
    pub name: &'static str,
    pub linux: PathBuf,
    pub native: PathBuf,
    pub linux_arguments: Vec<String>,
    pub native_arguments: Vec<String>,
}

impl Layout {
    fn new(name: &'static str, sqlite: bool, source: &Path, rootfs: &Path, output: &Path) -> Self {
        let linux = rootfs.join(format!("benchmark/malloc-{name}"));
        let native = output.join(format!("native/malloc-{name}"));
        let mut linux_arguments = vec!["-O3".into(), "-static".into()];
        let mut native_arguments = vec![
            "/mnt/mac/usr/bin/clang".into(),
            "-O3".into(),
            "-arch".into(),
            "x86_64".into(),
        ];
        if sqlite {
            linux_arguments.push("-DHL_SQLITE_LAYOUT".into());
            native_arguments.push("-DHL_SQLITE_LAYOUT".into());
        }
        linux_arguments.extend([source.display().to_string(), "-o".into(), linux.display().to_string()]);
        native_arguments.extend([mac_path(source), "-o".into(), mac_path(&native)]);
        if sqlite {
            linux_arguments.push("-lsqlite3".into());
            native_arguments.push("-lsqlite3".into());
        }
        Self { name, linux, native, linux_arguments, native_arguments }
    }
}

pub(super) fn layouts(source: &Path, rootfs: &Path, output: &Path) -> [Layout; 2] {
    [
        Layout::new("plain", false, source, rootfs, output),
        Layout::new("sqlite", true, source, rootfs, output),
    ]
}

pub(super) fn mac_path(path: &Path) -> String {
    format!("/mnt/mac{}", path.display())
}

#[cfg(test)]
mod tests {
    use super::layouts;

    #[test]
    fn plain_and_sqlite_layouts_share_one_source_and_differ_only_by_link_contract() {
        let layouts = layouts(
            std::path::Path::new("/workspace/tests/benchmark/three-arm/malloc.c"),
            std::path::Path::new("/stage/rootfs"),
            std::path::Path::new("/stage"),
        );
        assert_eq!(
            layouts.iter().map(|layout| layout.name).collect::<Vec<_>>(),
            ["plain", "sqlite"]
        );
        assert!(
            !layouts[0]
                .linux_arguments
                .iter()
                .any(|argument| argument.contains("sqlite"))
        );
        for arguments in [&layouts[1].linux_arguments, &layouts[1].native_arguments] {
            assert!(arguments.iter().any(|argument| argument == "-DHL_SQLITE_LAYOUT"));
            assert!(arguments.iter().any(|argument| argument == "-lsqlite3"));
        }
        for layout in layouts {
            assert!(
                layout
                    .linux_arguments
                    .iter()
                    .any(|argument| argument.ends_with("/malloc.c"))
            );
            assert!(
                layout
                    .native_arguments
                    .iter()
                    .any(|argument| argument.ends_with("/malloc.c"))
            );
        }
    }
}
