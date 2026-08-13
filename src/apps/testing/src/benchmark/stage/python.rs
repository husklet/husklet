use super::{Error, frame, mac, mac_path, require_parity};
use std::{fs, path::Path};

const MACOS_PYTHON: &str = "/mnt/mac/usr/bin/python3";
pub(super) const IMAGE: &str =
    "python:3.12-alpine@sha256:6d43704baacd1bfbe7c295d7f13079d5d8104ed33568873133f8fc69980419df";
pub(super) const IMAGE_ID: &str = "sha256:6d43704baacd1bfbe7c295d7f13079d5d8104ed33568873133f8fc69980419df";
const PLAIN_PROGRAM: &str = r#"import json,sys,time
if len(sys.argv)!=2:
 raise SystemExit('expected exactly one factors token')
parts=sys.argv[1].split(',')
if len(parts)!=2 or any(part not in ('1','2','4','8') for part in parts):
 raise SystemExit('factors must be <1|2|4|8>,<1|2|4|8>')
compute_factor,codec_factor=map(int,parts)
compute_expected={1:2666646666700000,2:21333253333400000,4:170666346666800000,8:1365332053333600000}
codec_expected={1:6172890675,2:12345783850,4:24691577700,8:49383195400}
started=time.monotonic_ns()
value=sum(i*i for i in range(200000*compute_factor))
compute=max(1,(time.monotonic_ns()-started)//1000)
assert value==compute_expected[compute_factor]
started=time.monotonic_ns()
proof=0
payload={'value':123456789,'words':['husklet']*1000}
for iteration in range(50*codec_factor):
 encoded=json.dumps(payload,sort_keys=True,separators=(',',':'))
 decoded=json.loads(encoded)
 proof+=decoded['value']+len(decoded['words'])+iteration
codec=max(1,(time.monotonic_ns()-started)//1000)
assert proof==codec_expected[codec_factor]
print(f'META workload=python layout=plain version=1 factors={compute_factor},{codec_factor}')
print(f'PHASE python-compute us={compute} ok={value}')
print(f'PHASE python-codec us={codec} ok={proof}')"#;
const SQLITE_PROGRAM: &str = r#"import sqlite3,sys,time
if len(sys.argv)!=2:
 raise SystemExit('expected exactly one factors token')
parts=sys.argv[1].split(',')
if len(parts)!=2 or any(part not in ('1','2','4','8') for part in parts):
 raise SystemExit('factors must be <1|2|4|8>,<1|2|4|8>')
write_factor,read_factor=map(int,parts)
write_expected={1:(20000,200010000),2:(40000,800020000),4:(80000,3200040000),8:(160000,12800080000)}
read_expected={1:4000200000,2:8000400000,4:16000800000,8:32001600000}
database=sqlite3.connect(':memory:')
database.execute('create table read_values(value integer not null)')
database.executemany('insert into read_values values (?)',((value,) for value in range(1,20001)))
database.commit()
started=time.monotonic_ns()
database.execute('create table values_(value integer not null)')
database.executemany('insert into values_ values (?)',((value,) for value in range(1,20000*write_factor+1)))
database.commit()
write=max(1,(time.monotonic_ns()-started)//1000)
write_proof=database.execute('select count(*),sum(value) from values_').fetchone()
assert write_proof==write_expected[write_factor]
started=time.monotonic_ns()
read_proof=0
for _ in range(20*read_factor):
 read_proof+=database.execute('select sum(value) from read_values').fetchone()[0]
read=max(1,(time.monotonic_ns()-started)//1000)
assert read_proof==read_expected[read_factor]
print(f'META workload=python layout=sqlite version=1 factors={write_factor},{read_factor}')
print(f'PHASE python-sqlite-write us={write} ok={write_proof[0]}:{write_proof[1]}')
print(f'PHASE python-sqlite-read us={read} ok={read_proof}')"#;

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

        for (layout, program, factors) in [
            ("plain", PLAIN_PROGRAM, "4,4"),
            ("sqlite", SQLITE_PROGRAM, "4,2"),
        ] {
            let native_output = mac(&[
                mac_path(arch_tool),
                "-x86_64".into(),
                mac_path(&interpreter),
                "-c".into(),
                program.into(),
                factors.into(),
            ])?;
            let linux_output = mac(&[
                mac_path(docker),
                "run".into(),
                "--rm".into(),
                "--platform".into(),
                "linux/amd64".into(),
                IMAGE.into(),
                "python3".into(),
                "-c".into(),
                program.into(),
                factors.into(),
            ])?;
            let native_frame = frame(&native_output)?;
            let linux_frame = frame(&linux_output)?;
            require_parity(&format!("python/{layout}"), &native_frame, &linux_frame)?;
            fs::write(output.join(format!("python-{layout}-native.out")), native_output)?;
            fs::write(output.join(format!("python-{layout}-linux.out")), linux_output)?;
            fs::write(output.join(format!("python-{layout}-exact-output.frame")), native_frame)?;
        }
        let sqlite_identity = mac(&[
            mac_path(arch_tool),
            "-x86_64".into(),
            mac_path(&interpreter),
            "-c".into(),
            "import sqlite3; print(sqlite3.sqlite_version)".into(),
        ])?;
        Ok(Self {
            interpreter,
            sqlite_identity: String::from_utf8(sqlite_identity)?.trim().to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{PLAIN_PROGRAM, SQLITE_PROGRAM};

    #[test]
    fn programs_require_two_strict_factors_and_independent_proofs() {
        for program in [PLAIN_PROGRAM, SQLITE_PROGRAM] {
            assert!(program.contains("len(sys.argv)!=2"));
            assert!(program.contains("part not in ('1','2','4','8')"));
            assert!(program.contains("factors={"));
        }
        assert!(PLAIN_PROGRAM.contains("compute_expected={1:2666646666700000"));
        assert!(PLAIN_PROGRAM.contains("codec_expected={1:6172890675"));
        assert!(SQLITE_PROGRAM.contains("write_expected={1:(20000,200010000)"));
        assert!(SQLITE_PROGRAM.contains("read_expected={1:4000200000"));
        assert!(SQLITE_PROGRAM.contains("read_values"));
    }
}
