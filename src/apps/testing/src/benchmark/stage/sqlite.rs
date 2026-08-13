use super::{Error, frame, husklet_rootfs_guest, mac, mac_path, require_parity};
use std::{fs, path::Path};

const MACOS_SQLITE: &str = "/mnt/mac/usr/bin/sqlite3";
const PACKAGE: &str = "sqlite=3.49.2-r1";
pub(super) const PROGRAM: &str = r#"CREATE TABLE values_(value INTEGER NOT NULL);
WITH RECURSIVE sequence(value) AS (VALUES(1) UNION ALL SELECT value+1 FROM sequence WHERE value<20000) INSERT INTO values_ SELECT value FROM sequence;
SELECT 'META workload=sqlite layout=sqlite version=1';
SELECT 'PHASE sqlite-write us=1 ok=' || count(*) FROM values_;
SELECT 'PHASE sqlite-read us=1 ok=' || sum(value) FROM values_;"#;

pub(super) struct SqliteProfile {
    pub command: std::path::PathBuf,
    pub linux_identity: String,
}

pub(super) struct SqliteHusklet {
    pub rootfs: std::path::PathBuf,
    pub interpreter: std::path::PathBuf,
}

impl SqliteProfile {
    pub(super) fn stage(output: &Path, docker: &Path, arch_tool: &Path) -> Result<Self, Error> {
        let command = output.join("native/sqlite3");
        let slices = mac(&["/mnt/mac/usr/bin/lipo".into(), "-archs".into(), MACOS_SQLITE.into()])?;
        if !std::str::from_utf8(&slices)?
            .split_ascii_whitespace()
            .any(|slice| slice == "x86_64")
        {
            return Err("macOS sqlite3 has no x86_64 slice".into());
        }
        mac(&["cp".into(), MACOS_SQLITE.into(), mac_path(&command)])?;

        let native_output = mac(&[
            mac_path(arch_tool),
            "-x86_64".into(),
            mac_path(&command),
            ":memory:".into(),
            PROGRAM.into(),
        ])?;
        let install_and_run = format!("apk add --no-cache {PACKAGE} >/dev/null && exec sqlite3 :memory: \"$1\"");
        let linux_output = mac(&[
            mac_path(docker),
            "run".into(),
            "--rm".into(),
            "--platform".into(),
            "linux/amd64".into(),
            super::IMAGE.into(),
            "sh".into(),
            "-c".into(),
            install_and_run,
            "sqlite-stage".into(),
            PROGRAM.into(),
        ])?;
        require_parity("sqlite/sqlite", &frame(&native_output)?, &frame(&linux_output)?)?;

        let linux_identity = mac(&[
            mac_path(docker),
            "run".into(),
            "--rm".into(),
            "--platform".into(),
            "linux/amd64".into(),
            super::IMAGE.into(),
            "sh".into(),
            "-c".into(),
            format!("apk add --no-cache {PACKAGE} >/dev/null && sha256sum /usr/bin/sqlite3 | cut -d' ' -f1"),
        ])?;
        let linux_identity = String::from_utf8(linux_identity)?.trim().to_owned();
        if linux_identity.len() != 64 || !linux_identity.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("Linux sqlite3 did not produce a sha256 identity".into());
        }
        fs::write(output.join("sqlite-native.out"), native_output)?;
        fs::write(output.join("sqlite-linux.out"), linux_output)?;
        fs::write(
            output.join("sqlite-exact-output.frame"),
            frame(&fs::read(output.join("sqlite-native.out"))?)?,
        )?;
        Ok(Self {
            command,
            linux_identity,
        })
    }

    pub(super) fn stage_husklet(
        output: &Path,
        rootfs: &Path,
        command: &Path,
        expected: &Path,
    ) -> Result<SqliteHusklet, Error> {
        let interpreter = rootfs.join("usr/bin/sqlite3");
        let captured = husklet_rootfs_guest(command, rootfs, "usr/bin/sqlite3", &[":memory:", PROGRAM])?;
        let actual = frame(&captured)?;
        require_parity("sqlite/sqlite Husklet", &fs::read(expected)?, &actual)?;
        fs::write(output.join("sqlite-husklet.out"), captured)?;
        fs::write(output.join("sqlite-husklet-exact-output.frame"), actual)?;
        Ok(SqliteHusklet {
            rootfs: rootfs.to_path_buf(),
            interpreter,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{PACKAGE, PROGRAM};

    #[test]
    fn sqlite_profile_is_pinned_and_has_write_and_read_proofs() {
        assert!(PACKAGE.contains('='));
        assert!(PROGRAM.contains("PHASE sqlite-write"));
        assert!(PROGRAM.contains("PHASE sqlite-read"));
        assert!(PROGRAM.contains("sum(value)"));
    }
}
