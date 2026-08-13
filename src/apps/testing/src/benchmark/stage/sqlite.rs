use super::{Error, frame, husklet_rootfs_guest, mac, mac_path, require_parity};
use std::{fs, path::Path};

pub(super) const SOURCE: &str = "src/apps/testing/tests/fixtures/guest/sqlite.c";
const AMALGAMATION_SHA256: &str = "41716b44ac8777188c4c3f1f370f01c9cb9e3b6428eb5c981d086c35de2d9d3f";
const SQLITE_C_SHA256: &str = "292cdfac26469d65501e4058c7a55ae0f811da78b2ae1e5c25db2ea44ae988f9";
const SQLITE_H_SHA256: &str = "cef9adf8b309ab3e903f1da5cda9f208cf5b901aa21e944df2dc04d9cd0ccee7";

pub(super) struct SqliteProfile {
    pub command: std::path::PathBuf,
    pub guest: std::path::PathBuf,
    pub linux_identity: String,
    pub source_identity: String,
}

pub(super) struct SqliteHusklet {
    pub rootfs: std::path::PathBuf,
    pub interpreter: std::path::PathBuf,
}

impl SqliteProfile {
    pub(super) fn stage(
        output: &Path,
        rootfs: &Path,
        source: &Path,
        amalgamation_archive: &Path,
        docker: &Path,
        arch_tool: &Path,
    ) -> Result<Self, Error> {
        let command = output.join("native/sqlite");
        let guest = rootfs.join("benchmark/sqlite");
        if !amalgamation_archive.is_absolute() || !amalgamation_archive.is_file() {
            return Err("--sqlite-amalgamation must name an absolute regular file".into());
        }
        if super::raw_sha256(amalgamation_archive)? != AMALGAMATION_SHA256 {
            return Err("SQLite amalgamation archive identity mismatch".into());
        }
        let amalgamation = output.join("sqlite-amalgamation");
        fs::create_dir(&amalgamation)?;
        mac(&[
            "/mnt/mac/usr/bin/ditto".into(),
            "-x".into(),
            "-k".into(),
            mac_path(amalgamation_archive),
            mac_path(&amalgamation),
        ])?;
        let sqlite_directory = amalgamation.join("sqlite-amalgamation-3500100");
        let sqlite_c = sqlite_directory.join("sqlite3.c");
        let sqlite_h = sqlite_directory.join("sqlite3.h");
        if super::raw_sha256(&sqlite_c)? != SQLITE_C_SHA256 || super::raw_sha256(&sqlite_h)? != SQLITE_H_SHA256 {
            return Err("extracted SQLite amalgamation identity mismatch".into());
        }
        let sqlite_object = output.join("native/sqlite3.o");
        let fixture_object = output.join("native/sqlite-fixture.o");
        mac(&[
            "/mnt/mac/usr/bin/clang".into(),
            "-O3".into(),
            "-DSQLITE_THREADSAFE=0".into(),
            "-DSQLITE_OMIT_LOAD_EXTENSION".into(),
            "-arch".into(),
            "x86_64".into(),
            "-c".into(),
            mac_path(&sqlite_c),
            "-o".into(),
            mac_path(&sqlite_object),
        ])?;
        mac(&[
            "/mnt/mac/usr/bin/clang".into(),
            "-O3".into(),
            "-Wall".into(),
            "-Wextra".into(),
            "-Werror".into(),
            "-Wconversion".into(),
            "-Wshadow".into(),
            "-arch".into(),
            "x86_64".into(),
            "-I".into(),
            mac_path(&sqlite_directory),
            "-c".into(),
            mac_path(source),
            "-o".into(),
            mac_path(&fixture_object),
        ])?;
        mac(&[
            "/mnt/mac/usr/bin/clang".into(),
            "-arch".into(),
            "x86_64".into(),
            mac_path(&sqlite_object),
            mac_path(&fixture_object),
            "-o".into(),
            mac_path(&command),
        ])?;
        let linux_build = "apk add --no-cache build-base=0.5-r3 >/dev/null && cc -O3 -DSQLITE_THREADSAFE=0 -DSQLITE_OMIT_LOAD_EXTENSION -c /sqlite/sqlite3.c -o /tmp/sqlite3.o && cc -O3 -Wall -Wextra -Werror -Wconversion -Wshadow -I/sqlite -c /source.c -o /tmp/fixture.o && cc -static /tmp/sqlite3.o /tmp/fixture.o -o /out/sqlite";
        mac(&[
            mac_path(docker),
            "run".into(),
            "--rm".into(),
            "--platform".into(),
            "linux/amd64".into(),
            "--mount".into(),
            format!("type=bind,source={},target=/source.c,readonly", mac_path(source)),
            "--mount".into(),
            format!(
                "type=bind,source={},target=/sqlite,readonly",
                mac_path(&sqlite_directory)
            ),
            "--mount".into(),
            format!("type=bind,source={},target=/out", mac_path(&rootfs.join("benchmark"))),
            super::IMAGE.into(),
            "sh".into(),
            "-c".into(),
            linux_build.into(),
        ])?;

        let native_output = mac(&[mac_path(arch_tool), "-x86_64".into(), mac_path(&command)])?;
        let linux_output = mac(&[
            mac_path(docker),
            "run".into(),
            "--rm".into(),
            "--platform".into(),
            "linux/amd64".into(),
            "--mount".into(),
            format!(
                "type=bind,source={},target={},readonly",
                mac_path(rootfs),
                rootfs.display()
            ),
            super::IMAGE.into(),
            guest.display().to_string(),
        ])?;
        require_parity(
            "sqlite/sqlite",
            &profile_frame(&native_output)?,
            &profile_frame(&linux_output)?,
        )?;

        let linux_identity = super::raw_sha256(&guest)?;
        fs::write(output.join("sqlite-native.out"), native_output)?;
        fs::write(output.join("sqlite-linux.out"), linux_output)?;
        fs::write(
            output.join("sqlite-exact-output.frame"),
            profile_frame(&fs::read(output.join("sqlite-native.out"))?)?,
        )?;
        Ok(Self {
            command,
            guest,
            linux_identity,
            source_identity: SQLITE_C_SHA256.into(),
        })
    }

    pub(super) fn stage_husklet(
        output: &Path,
        rootfs: &Path,
        command: &Path,
        expected: &Path,
    ) -> Result<SqliteHusklet, Error> {
        let interpreter = rootfs.join("benchmark/sqlite");
        let captured = husklet_rootfs_guest(command, rootfs, "benchmark/sqlite", &[])?;
        let actual = profile_frame(&captured)?;
        require_parity("sqlite/sqlite Husklet", &fs::read(expected)?, &actual)?;
        fs::write(output.join("sqlite-husklet.out"), captured)?;
        fs::write(output.join("sqlite-husklet-exact-output.frame"), actual)?;
        Ok(SqliteHusklet {
            rootfs: rootfs.to_path_buf(),
            interpreter,
        })
    }
}

pub(super) fn profile_frame(output: &[u8]) -> Result<Vec<u8>, Error> {
    let text = std::str::from_utf8(output)?;
    for phase in ["sqlite-write", "sqlite-read"] {
        let prefix = format!("PHASE {phase} us=");
        let line = text
            .lines()
            .find(|line| line.starts_with(&prefix))
            .ok_or_else(|| format!("sqlite workload omitted {phase}"))?;
        let elapsed = line[prefix.len()..]
            .split_ascii_whitespace()
            .next()
            .ok_or("sqlite phase omitted its duration")?
            .parse::<u64>()?;
        if elapsed <= 1 {
            return Err(format!("sqlite {phase} duration was not greater than one microsecond").into());
        }
    }
    frame(output)
}

#[cfg(test)]
mod tests {
    use super::{SOURCE, profile_frame};

    #[test]
    fn sqlite_fixture_measures_write_and_read_with_stable_proofs() {
        let source = include_str!("../../../tests/fixtures/guest/sqlite.c");
        assert!(SOURCE.ends_with("/sqlite.c"));
        assert!(source.contains("clock_gettime(CLOCK_MONOTONIC"));
        assert!(source.contains("PHASE sqlite-write us=%llu ok=%lld"));
        assert!(source.contains("PHASE sqlite-read us=%llu ok=%lld"));
        assert!(source.contains("READ_SCANS = 50"));
        assert!(source.contains("checksum != INT64_C(200010000)"));
        assert!(source.contains("square_checksum != INT64_C(2666866670000)"));
        assert!(source.contains("write <= 1 || read <= 1"));
    }

    #[test]
    fn sqlite_frame_rejects_constant_or_zero_duration() {
        let valid = b"META workload=sqlite layout=sqlite version=1\nPHASE sqlite-write us=2 ok=20000\nPHASE sqlite-read us=3 ok=20000:200010000:2666866670000\n";
        assert!(profile_frame(valid).is_ok());
        for invalid in [
            b"META workload=sqlite layout=sqlite version=1\nPHASE sqlite-write us=1 ok=20000\nPHASE sqlite-read us=3 ok=proof\n".as_slice(),
            b"META workload=sqlite layout=sqlite version=1\nPHASE sqlite-write us=2 ok=20000\nPHASE sqlite-read us=0 ok=proof\n".as_slice(),
        ] {
            assert!(profile_frame(invalid).is_err());
        }
    }
}
