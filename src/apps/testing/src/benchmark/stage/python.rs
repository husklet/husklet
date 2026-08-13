use super::{Error, frame, mac, mac_path, require_parity};
use std::{fs, path::Path};

const MACOS_PYTHON: &str = "/mnt/mac/usr/bin/python3";
pub(super) const IMAGE: &str =
    "python:3.12-alpine@sha256:6d43704baacd1bfbe7c295d7f13079d5d8104ed33568873133f8fc69980419df";
pub(super) const IMAGE_ID: &str = "sha256:6d43704baacd1bfbe7c295d7f13079d5d8104ed33568873133f8fc69980419df";
pub(super) const PLAIN_PROGRAM: &str = r#"import json,time
started=time.monotonic_ns()
value=sum(i*i for i in range(1000000))
compute=max(1,(time.monotonic_ns()-started)//1000)
assert value==333332833333500000
started=time.monotonic_ns()
payload={'value':value,'words':['husklet']*2000}
proof=0
for iteration in range(200):
 encoded=json.dumps(payload,sort_keys=True,separators=(',',':'))
 decoded=json.loads(encoded)
 proof+=decoded['value']+len(decoded['words'])+iteration
codec=max(1,(time.monotonic_ns()-started)//1000)
assert proof==66666566666700419900
print('META workload=python layout=plain version=1')
print(f'PHASE python-compute us={compute} ok={value}')
print(f'PHASE python-codec us={codec} ok={proof}')"#;
pub(super) const SQLITE_PROGRAM: &str = r#"import sqlite3,time
database=sqlite3.connect(':memory:')
started=time.monotonic_ns()
database.execute('create table values_(value integer not null)')
database.executemany('insert into values_ values (?)',((value,) for value in range(1,50001)))
database.commit()
write=max(1,(time.monotonic_ns()-started)//1000)
write_proof=database.execute('select count(*) from values_').fetchone()[0]
assert write_proof==50000
started=time.monotonic_ns()
count=total=squares=0
for _ in range(40):
 row=database.execute('select count(*),sum(value),sum(value*value) from values_').fetchone()
 count+=row[0]
 total+=row[1]
 squares+=row[2]
read=max(1,(time.monotonic_ns()-started)//1000)
assert (count,total,squares)==(2000000,50001000000,1666716667000000)
print('META workload=python layout=sqlite version=1')
print(f'PHASE python-sqlite-write us={write} ok={write_proof}')
print(f'PHASE python-sqlite-read us={read} ok={count}:{total}:{squares}')"#;

const MINIMUM_PHASE_MICROS: u64 = 5_000;

pub(super) struct PythonProfile {
    pub interpreter: std::path::PathBuf,
    pub sqlite_identity: String,
}

impl PythonProfile {
    pub(super) fn stage(output: &Path, docker: &Path, arch_tool: &Path) -> Result<Self, Error> {
        let interpreter = output.join("native/python3");
        let slices = mac(&["/mnt/mac/usr/bin/lipo".into(), "-archs".into(), MACOS_PYTHON.into()])?;
        if !std::str::from_utf8(&slices)?
            .split_ascii_whitespace()
            .any(|slice| slice == "x86_64")
        {
            return Err("macOS /usr/bin/python3 has no x86_64 slice".into());
        }
        mac(&["cp".into(), MACOS_PYTHON.into(), mac_path(&interpreter)])?;
        let copied_slices = mac(&["/mnt/mac/usr/bin/lipo".into(), "-archs".into(), mac_path(&interpreter)])?;
        if !std::str::from_utf8(&copied_slices)?
            .split_ascii_whitespace()
            .any(|slice| slice == "x86_64")
        {
            return Err("staged macOS Python lost its x86_64 slice".into());
        }

        for (layout, program) in [("plain", PLAIN_PROGRAM), ("sqlite", SQLITE_PROGRAM)] {
            let native_output = mac(&[
                mac_path(arch_tool),
                "-x86_64".into(),
                mac_path(&interpreter),
                "-B".into(),
                "-c".into(),
                program.into(),
            ])?;
            let linux_output = mac(&[
                mac_path(docker),
                "run".into(),
                "--rm".into(),
                "--platform".into(),
                "linux/amd64".into(),
                IMAGE.into(),
                "python3".into(),
                "-B".into(),
                "-c".into(),
                program.into(),
            ])?;
            let native_frame = profile_frame(layout, &native_output)?;
            let linux_frame = profile_frame(layout, &linux_output)?;
            require_parity(&format!("python/{layout}"), &native_frame, &linux_frame)?;
            fs::write(output.join(format!("python-{layout}-native.out")), native_output)?;
            fs::write(output.join(format!("python-{layout}-linux.out")), linux_output)?;
            fs::write(output.join(format!("python-{layout}-exact-output.frame")), native_frame)?;
        }
        let sqlite_identity = mac(&[
            mac_path(arch_tool),
            "-x86_64".into(),
            mac_path(&interpreter),
            "-B".into(),
            "-c".into(),
            "import sqlite3; print(sqlite3.sqlite_version)".into(),
        ])?;
        Ok(Self {
            interpreter,
            sqlite_identity: String::from_utf8(sqlite_identity)?.trim().to_owned(),
        })
    }
}

pub(super) fn profile_frame(layout: &str, output: &[u8]) -> Result<Vec<u8>, Error> {
    let phases: &[&str] = match layout {
        "plain" => &["python-compute", "python-codec"],
        "sqlite" => &["python-sqlite-write", "python-sqlite-read"],
        _ => return Err(format!("unknown Python benchmark layout {layout}").into()),
    };
    let text = std::str::from_utf8(output)?;
    for phase in phases {
        let prefix = format!("PHASE {phase} us=");
        let line = text
            .lines()
            .find(|line| line.starts_with(&prefix))
            .ok_or_else(|| format!("Python workload omitted {phase}"))?;
        let elapsed = line[prefix.len()..]
            .split_ascii_whitespace()
            .next()
            .ok_or_else(|| format!("Python phase {phase} omitted its duration"))?
            .parse::<u64>()?;
        if elapsed < MINIMUM_PHASE_MICROS {
            return Err(format!("Python phase {phase} was shorter than the 5 ms smoke floor").into());
        }
    }
    frame(output)
}

#[cfg(test)]
mod tests {
    use super::{PLAIN_PROGRAM, SQLITE_PROGRAM, profile_frame};

    #[test]
    fn programs_measure_repeated_operations_with_stable_proofs() {
        assert!(PLAIN_PROGRAM.contains("range(1000000)"));
        assert!(PLAIN_PROGRAM.contains("range(200)"));
        assert!(PLAIN_PROGRAM.contains("assert value==333332833333500000"));
        assert!(PLAIN_PROGRAM.contains("assert proof==66666566666700419900"));
        assert!(SQLITE_PROGRAM.contains("range(1,50001)"));
        assert!(SQLITE_PROGRAM.contains("range(40)"));
        assert!(SQLITE_PROGRAM.contains("select count(*),sum(value),sum(value*value)"));
        assert!(SQLITE_PROGRAM.contains("(2000000,50001000000,1666716667000000)"));
    }

    #[test]
    fn profile_rejects_phases_below_five_milliseconds() {
        let valid = b"META workload=python layout=plain version=1\nPHASE python-compute us=5000 ok=333332833333500000\nPHASE python-codec us=5001 ok=proof\n";
        assert!(profile_frame("plain", valid).is_ok());
        for invalid in [
            b"META workload=python layout=plain version=1\nPHASE python-compute us=4999 ok=proof\nPHASE python-codec us=5001 ok=proof\n".as_slice(),
            b"META workload=python layout=plain version=1\nPHASE python-compute us=5001 ok=proof\nPHASE python-codec us=0 ok=proof\n".as_slice(),
        ] {
            assert!(profile_frame("plain", invalid).is_err());
        }
    }
}
