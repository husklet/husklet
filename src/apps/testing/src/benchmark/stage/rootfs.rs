use super::{Error, mac, mac_path, python};
use std::{fs, path::Path};

const SQLITE_PACKAGE: &str = "sqlite=3.53.2-r0";

pub(super) fn stage(output: &Path, docker: &Path) -> Result<(), Error> {
    let rootfs = output.join("rootfs");
    let archive = output.join("rootfs.tar");
    fs::create_dir(&rootfs)?;
    let created = mac(&[
        mac_path(docker),
        "create".into(),
        "--platform".into(),
        "linux/amd64".into(),
        python::IMAGE.into(),
        "sh".into(),
        "-c".into(),
        install_command(),
    ])?;
    let container = String::from_utf8(created)?.trim().to_owned();
    mac(&[mac_path(docker), "start".into(), "--attach".into(), container.clone()])?;
    mac(&[
        mac_path(docker),
        "export".into(),
        "--output".into(),
        mac_path(&archive),
        container.clone(),
    ])?;
    mac(&[mac_path(docker), "rm".into(), container])?;
    mac(&[
        "/mnt/mac/usr/bin/tar".into(),
        "-xf".into(),
        mac_path(&archive),
        "-C".into(),
        mac_path(&rootfs),
    ])?;
    fs::remove_file(archive)?;
    fs::create_dir(rootfs.join("benchmark"))?;
    for path in ["usr/local/bin/python3.12", "usr/bin/sqlite3"] {
        if !rootfs.join(path).is_file() {
            return Err(format!("unified benchmark rootfs is missing {path}").into());
        }
    }
    Ok(())
}

fn install_command() -> String {
    format!("apk add --no-cache {SQLITE_PACKAGE} >/dev/null")
}

#[cfg(test)]
mod tests {
    use super::install_command;

    #[test]
    fn unified_rootfs_install_is_version_pinned() {
        let command = install_command();
        assert!(command.starts_with("apk add --no-cache sqlite="));
        assert!(command.ends_with(" >/dev/null"));
    }
}
