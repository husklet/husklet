//! Public filesystem coherence contracts against a running Linux process.

use hl_container::{Config, ContainerSpec, Containers, ExitStatus, Guest, Isolation, Limits, Process, Sandbox};
use std::{io::Cursor, path::Path, time::Duration};

type Error = Box<dyn std::error::Error>;

struct Case {
    name: &'static str,
    command: &'static str,
    destination: &'static str,
    entries: &'static [(&'static str, &'static [u8])],
    expected: &'static [&'static str],
}

#[tokio::test]
#[ignore = "requires HL_ALPINE_ARCHIVE"]
async fn new_file_is_visible() -> Result<(), Error> {
    run(Case {
        name: "copy-new",
        command: "mkdir -p /tmp/cp-new; touch /tmp/cp-ready; i=0; while [ $i -lt 400 ]; do if [ -e /tmp/cp-new/probe ]; then echo SEEN:$(cat /tmp/cp-new/probe); exit 0; fi; i=$((i+1)); sleep .1; done; echo TIMEOUT; exit 1",
        destination: "/tmp/cp-new",
        entries: &[("probe", b"hello-cp\n")],
        expected: &["SEEN:hello-cp"],
    })
    .await
}

#[tokio::test]
#[ignore = "requires HL_ALPINE_ARCHIVE"]
async fn overwritten_file_is_visible() -> Result<(), Error> {
    run(Case {
        name: "copy-overwrite",
        command: "mkdir -p /tmp/cp-over; : > /tmp/cp-over/probe; touch /tmp/cp-ready; i=0; while [ $i -lt 400 ]; do if [ -s /tmp/cp-over/probe ]; then echo GREW:$(cat /tmp/cp-over/probe); exit 0; fi; i=$((i+1)); sleep .1; done; echo TIMEOUT; exit 1",
        destination: "/tmp/cp-over",
        entries: &[("probe", b"new-content\n")],
        expected: &["GREW:new-content"],
    })
    .await
}

#[tokio::test]
#[ignore = "requires HL_ALPINE_ARCHIVE"]
async fn directory_tree_is_visible() -> Result<(), Error> {
    run(Case {
        name: "copy-tree",
        command: "mkdir -p /tmp/cp-tree; touch /tmp/cp-ready; i=0; while [ $i -lt 400 ]; do if [ -e /tmp/cp-tree/d/sub/leaf ]; then echo TREE:$(cat /tmp/cp-tree/d/sub/leaf); exit 0; fi; i=$((i+1)); sleep .1; done; echo TIMEOUT; exit 1",
        destination: "/tmp/cp-tree",
        entries: &[("d/sub/leaf", b"LEAF-CONTENT\n")],
        expected: &["TREE:LEAF-CONTENT"],
    })
    .await
}

#[tokio::test]
#[ignore = "requires HL_ALPINE_ARCHIVE"]
async fn held_directory_is_coherent() -> Result<(), Error> {
    run(Case {
        name: "copy-held",
        command: "mkdir -p /tmp/cp-held; cd /tmp/cp-held; touch /tmp/cp-ready; i=0; while [ $i -lt 400 ]; do if [ -e ./probe ]; then echo HELD:$(cat ./probe); echo LIST:$(ls); exit 0; fi; i=$((i+1)); sleep .1; done; echo TIMEOUT; exit 1",
        destination: "/tmp/cp-held",
        entries: &[("probe", b"held-content\n")],
        expected: &["HELD:held-content", "LIST:probe"],
    })
    .await
}

async fn run(case: Case) -> Result<(), Error> {
    let work = tempfile::tempdir()?;
    let rootfs = work.path().join("rootfs");
    unpack(&rootfs)?;
    let containers = Containers::builder(Config::new(work.path().join("state")))
        .build()
        .await?;
    let spec = ContainerSpec::from_directory(&rootfs, Process::new("/bin/sh").args(["-c", case.command]))
        .name(case.name)
        .guest(guest()?)
        .isolation(Isolation {
            sandbox: Sandbox::Disabled,
            ..Isolation::default()
        });
    containers.create(spec).await?;
    containers.start(case.name).await?;

    let outcome = execute(&containers, &rootfs, &case).await;
    let cleanup = containers.remove_force(case.name).await.map(|_| ());
    match (outcome, cleanup) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error.into()),
        (Ok(()), Ok(())) => Ok(()),
    }
}

async fn execute(containers: &Containers, rootfs: &Path, case: &Case) -> Result<(), Error> {
    let ready = rootfs.join("tmp/cp-ready");
    let ready_result = tokio::time::timeout(Duration::from_secs(5), async {
        while !ready.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    if ready_result.is_err() {
        let state = containers.inspect(case.name).await?;
        let logs = containers.logs(case.name).await?;
        return Err(format!(
            "running process did not publish its copy-ready marker: state={:?} stdout={:?} stderr={:?}",
            state.state,
            String::from_utf8_lossy(&logs.stdout),
            String::from_utf8_lossy(&logs.stderr),
        )
        .into());
    }
    std::fs::remove_file(ready)?;

    containers.filesystem(case.name).await?.extract(
        case.destination,
        Cursor::new(archive(case.entries)?),
        Limits::default(),
    )?;
    let status = tokio::time::timeout(Duration::from_secs(45), containers.wait(case.name))
        .await
        .map_err(|_| "running process did not observe the extracted files")??;
    let output = String::from_utf8(containers.logs(case.name).await?.stdout)?;
    if status != ExitStatus::Code(0) || case.expected.iter().any(|line| !output.lines().any(|got| got == *line)) {
        return Err(format!("status={status:?} stdout={output:?}").into());
    }
    Ok(())
}

fn guest() -> Result<Guest, Error> {
    match std::env::var("HL_SCENARIO_TARGET") {
        Ok(value) if value == "amd64" => Ok(Guest::X86_64),
        Ok(value) if value == "arm64" => Ok(Guest::Aarch64),
        Err(std::env::VarError::NotPresent) => Ok(Guest::Aarch64),
        Ok(value) => Err(format!("unsupported HL_SCENARIO_TARGET {value:?}").into()),
        Err(error) => Err(error.into()),
    }
}

fn unpack(destination: &Path) -> Result<(), Error> {
    let source = std::env::var_os("HL_ALPINE_ARCHIVE").ok_or("HL_ALPINE_ARCHIVE must name the pinned rootfs")?;
    std::fs::create_dir(destination)?;
    let archive = std::fs::File::open(source)?;
    tar::Archive::new(flate2::read::GzDecoder::new(archive)).unpack(destination)?;
    Ok(())
}

fn archive(entries: &[(&str, &[u8])]) -> Result<Vec<u8>, std::io::Error> {
    let mut bytes = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut bytes);
        for (path, contents) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_cksum();
            archive.append_data(&mut header, path, *contents)?;
        }
        archive.finish()?;
    }
    Ok(bytes)
}
