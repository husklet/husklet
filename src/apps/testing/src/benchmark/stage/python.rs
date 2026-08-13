use super::{Error, frame, mac, mac_path, require_parity};
use std::{fs, path::Path};

const MACOS_PYTHON: &str = "/mnt/mac/usr/bin/python3";
pub(super) const IMAGE: &str =
    "python:3.12-alpine@sha256:6d43704baacd1bfbe7c295d7f13079d5d8104ed33568873133f8fc69980419df";
pub(super) const IMAGE_ID: &str = "sha256:6d43704baacd1bfbe7c295d7f13079d5d8104ed33568873133f8fc69980419df";
const PLAIN_PROGRAM: &str = r#"import json,time
started=time.monotonic_ns()
value=sum(i*i for i in range(200000))
compute=max(1,(time.monotonic_ns()-started)//1000)
started=time.monotonic_ns()
encoded=json.dumps({'value':value,'words':['husklet']*1000},sort_keys=True,separators=(',',':'))
decoded=json.loads(encoded)
codec=max(1,(time.monotonic_ns()-started)//1000)
proof=decoded['value']+len(decoded['words'])
print('META workload=python layout=plain version=1')
print(f'PHASE python-compute us={compute} ok={proof}')
print(f'PHASE python-codec us={codec} ok={proof}')"#;
const SQLITE_PROGRAM: &str = r#"import sqlite3,time
database=sqlite3.connect(':memory:')
started=time.monotonic_ns()
database.execute('create table values_(value integer not null)')
database.executemany('insert into values_ values (?)',((value,) for value in range(1,20001)))
database.commit()
write=max(1,(time.monotonic_ns()-started)//1000)
started=time.monotonic_ns()
proof=database.execute('select sum(value) from values_').fetchone()[0]
read=max(1,(time.monotonic_ns()-started)//1000)
print('META workload=python layout=sqlite version=1')
print(f'PHASE python-sqlite-write us={write} ok={proof}')
print(f'PHASE python-sqlite-read us={read} ok={proof}')"#;

pub(super) struct Staged {
    pub interpreter: std::path::PathBuf,
    pub sqlite_identity: String,
}

pub(super) fn stage(output: &Path, docker: &Path, arch_tool: &Path) -> Result<Staged, Error> {
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
            "-c".into(),
            program.into(),
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
    Ok(Staged {
        interpreter,
        sqlite_identity: String::from_utf8(sqlite_identity)?.trim().to_owned(),
    })
}
