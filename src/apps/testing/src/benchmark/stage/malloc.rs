use super::{Error, mac};
use std::path::{Path, PathBuf};

pub(super) const SOURCE: &str = "src/apps/testing/tests/fixtures/guest/malloc.c";

pub(super) struct Layout {
    pub name: &'static str,
    pub linux: PathBuf,
    pub native: PathBuf,
    pub native_arguments: Vec<String>,
}

impl Layout {
    fn new(name: &'static str, sqlite: bool, source: &Path, rootfs: &Path, output: &Path) -> Self {
        let linux = rootfs.join(format!("benchmark/malloc-{name}"));
        let native = output.join(format!("native/malloc-{name}"));
        let mut native_arguments = vec![
            "/mnt/mac/usr/bin/clang".into(),
            "-O3".into(),
            "-arch".into(),
            "x86_64".into(),
        ];
        if sqlite {
            native_arguments.push("-DHL_SQLITE_LAYOUT".into());
        }
        native_arguments.extend([mac_path(source), "-o".into(), mac_path(&native)]);
        if sqlite {
            native_arguments.push("-lsqlite3".into());
        }
        Self {
            name,
            linux,
            native,
            native_arguments,
        }
    }
}

pub(super) fn build_linux(layout: &Layout, source: &Path, rootfs: &Path, docker: &Path) -> Result<(), Error> {
    let sqlite = if layout.name == "sqlite" {
        " -DHL_SQLITE_LAYOUT -lsqlite3"
    } else {
        ""
    };
    let command = format!(
        "apk add --no-cache build-base sqlite-dev sqlite-static >/dev/null && cc -O3 -static /source.c -o /out/{}{}",
        layout
            .linux
            .file_name()
            .ok_or("Linux workload has no filename")?
            .to_string_lossy(),
        sqlite
    );
    mac(&[
        mac_path(docker),
        "run".into(),
        "--rm".into(),
        "--platform".into(),
        "linux/amd64".into(),
        "--mount".into(),
        format!("type=bind,source={},target=/source.c,readonly", mac_path(source)),
        "--mount".into(),
        format!("type=bind,source={},target=/out", mac_path(&rootfs.join("benchmark"))),
        super::IMAGE.into(),
        "sh".into(),
        "-c".into(),
        command,
    ])?;
    Ok(())
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
            std::path::Path::new("/workspace/src/apps/testing/tests/fixtures/guest/malloc.c"),
            std::path::Path::new("/stage/rootfs"),
            std::path::Path::new("/stage"),
        );
        assert_eq!(
            layouts.iter().map(|layout| layout.name).collect::<Vec<_>>(),
            ["plain", "sqlite"]
        );
        assert!(
            !layouts[0]
                .native_arguments
                .iter()
                .any(|argument| argument.contains("sqlite"))
        );
        assert!(
            layouts[1]
                .native_arguments
                .iter()
                .any(|argument| argument == "-DHL_SQLITE_LAYOUT")
        );
        assert!(
            layouts[1]
                .native_arguments
                .iter()
                .any(|argument| argument == "-lsqlite3")
        );
        for layout in layouts {
            assert!(
                layout
                    .native_arguments
                    .iter()
                    .any(|argument| argument.ends_with("/malloc.c"))
            );
        }
    }
}
