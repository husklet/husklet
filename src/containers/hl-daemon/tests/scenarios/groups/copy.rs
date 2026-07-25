//! `docker cp` direction and live-filesystem coherence compatibility.

use crate::{contract::Target, report::ScenarioBatch};
use hl_container::{ContainerSpec, Containers, ExitStatus, Isolation, Limits, Process, Sandbox};
use std::{io::Cursor, path::Path, time::Duration};

type Error = Box<dyn std::error::Error>;

const IDS: [&str; 12] = [
    "cpcmd/host-to-container-file",
    "cpcmd/container-to-host-file",
    "cpcmd/host-to-container-dir",
    "cpcmd/container-to-host-dir",
    "cpcoherence/cp-new-file-live-poll",
    "cpcoherence/cp-new-file-live-poll.amd",
    "cpcoherence/cp-overwrite-cached-positive",
    "cpcoherence/cp-overwrite-cached-positive.amd",
    "cpcoherence/cp-dir-tree-live-poll",
    "cpcoherence/cp-dir-tree-live-poll.amd",
    "cpcoherence/cp-into-held-open-dir",
    "cpcoherence/cp-into-held-open-dir.amd",
];

pub(crate) async fn run(containers: &Containers, rootfs: &Path) -> Result<(), Error> {
    let target = Target::from_env()?;
    let selected = std::env::var("HL_SCENARIO_CASE").ok();
    let scenarios = crate::registry::copy::group()
        .scenarios
        .into_iter()
        .chain(crate::coherence::group().scenarios)
        .map(|value| (value.id, value))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut reports = ScenarioBatch::new("copy")?;
    let mut failures = Vec::new();
    let ids = IDS
        .into_iter()
        .filter(|id| selected.as_deref().is_none_or(|selected| selected == *id))
        .collect::<Vec<_>>();
    for id in &ids {
        let scenario = &scenarios[id];
        let Some(attempt) = reports.begin(scenario)? else {
            println!("RESUME {id}");
            continue;
        };
        if !scenario.targets.contains(&target) {
            reports.skip(scenario, attempt)?;
            println!("SKIP {id}");
            continue;
        }
        let result = Case {
            containers,
            rootfs,
            target,
        }
        .run(id)
        .await;
        reports.complete(scenario, attempt, &result)?;
        match result {
            Ok(()) => println!("PASS {id}"),
            Err(error) => {
                println!("FAIL {id}: {error}");
                failures.push(format!("{id}: {error}"));
            }
        }
    }
    println!(
        "copy scenarios: {} passed; {} failed; {} total",
        ids.len() - failures.len(),
        failures.len(),
        ids.len()
    );
    reports.finish(Vec::new())?;
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n").into())
    }
}

struct Case<'a> {
    containers: &'a Containers,
    rootfs: &'a Path,
    target: Target,
}

impl Case<'_> {
    async fn run(&self, id: &str) -> Result<(), Error> {
        let id = id.strip_suffix(".amd").unwrap_or(id);
        match id {
            "cpcmd/host-to-container-file" => {
                self.simple(id, "sleep 60", "/tmp", &[("f", b"CPFILE\n")], "/tmp/f", b"CPFILE\n").await
            }
            "cpcmd/container-to-host-file" => {
                self.simple(id, "echo FROMCTR > /tmp/g; touch /tmp/cp-ready; sleep 60", "/tmp", &[], "/tmp/g", b"FROMCTR\n").await
            }
            "cpcmd/host-to-container-dir" => {
                self.simple(id, "sleep 60", "/tmp", &[("d/a", b"AAA\n"), ("d/b", b"BBB\n")], "/tmp/d", b"AAA\nBBB\n").await
            }
            "cpcmd/container-to-host-dir" => {
                self.simple(id, "mkdir -p /tmp/e; echo XXX > /tmp/e/x; echo YYY > /tmp/e/y; touch /tmp/cp-ready; sleep 60", "/tmp", &[], "/tmp/e", b"XXX\nYYY\n").await
            }
            "cpcoherence/cp-new-file-live-poll" => self.live(id, "mkdir -p /tmp/cp-new; touch /tmp/cp-ready; i=0; while [ $i -lt 400 ]; do if [ -e /tmp/cp-new/probe ]; then echo SEEN:$(cat /tmp/cp-new/probe); exit 0; fi; i=$((i+1)); sleep .1; done; echo TIMEOUT; exit 1", "/tmp/cp-new", &[("probe", b"hello-cp\n")], &["SEEN:hello-cp"]).await,
            "cpcoherence/cp-overwrite-cached-positive" => self.live(id, "mkdir -p /tmp/cp-over; : > /tmp/cp-over/probe; touch /tmp/cp-ready; i=0; while [ $i -lt 400 ]; do if [ -s /tmp/cp-over/probe ]; then echo GREW:$(cat /tmp/cp-over/probe); exit 0; fi; i=$((i+1)); sleep .1; done; echo TIMEOUT; exit 1", "/tmp/cp-over", &[("probe", b"new-content\n")], &["GREW:new-content"]).await,
            "cpcoherence/cp-dir-tree-live-poll" => self.live(id, "mkdir -p /tmp/cp-tree; touch /tmp/cp-ready; i=0; while [ $i -lt 400 ]; do if [ -e /tmp/cp-tree/d/sub/leaf ]; then echo TREE:$(cat /tmp/cp-tree/d/sub/leaf); exit 0; fi; i=$((i+1)); sleep .1; done; echo TIMEOUT; exit 1", "/tmp/cp-tree", &[("d/sub/leaf", b"LEAF-CONTENT\n")], &["TREE:LEAF-CONTENT"]).await,
            "cpcoherence/cp-into-held-open-dir" => self.live(id, "mkdir -p /tmp/cp-held; cd /tmp/cp-held; touch /tmp/cp-ready; i=0; while [ $i -lt 400 ]; do if [ -e ./probe ]; then echo HELD:$(cat ./probe); echo LIST:$(ls); exit 0; fi; i=$((i+1)); sleep .1; done; echo TIMEOUT; exit 1", "/tmp/cp-held", &[("probe", b"held-content\n")], &["HELD:held-content", "LIST:probe"]).await,
            _ => unreachable!(),
        }
    }

    async fn create(&self, id: &str, command: &str) -> Result<String, Error> {
        let name = id.replace(['/', '.'], "-");
        self.containers
            .create(
                ContainerSpec::from_directory(
                    self.rootfs,
                    Process::new("/bin/sh").args(["-c", command]),
                )
                .name(&name)
                .guest(self.target.guest())
                .isolation(Isolation {
                    sandbox: Sandbox::Disabled,
                    ..Isolation::default()
                }),
            )
            .await?;
        self.containers.start(&name).await?;
        Ok(name)
    }

    async fn simple(
        &self,
        id: &str,
        command: &str,
        destination: &str,
        entries: &[(&str, &[u8])],
        source: &str,
        expected: &[u8],
    ) -> Result<(), Error> {
        let name = self.create(id, command).await?;
        if entries.is_empty() {
            self.ready().await?;
        } else {
            self.extract(&name, destination, entries).await?;
        }
        self.containers.stop(&name, Duration::ZERO).await?;
        let payload = self.archive(&name, source).await?;
        let mut payload_lines = payload.split(|byte| *byte == b'\n').collect::<Vec<_>>();
        let mut expected_lines = expected.split(|byte| *byte == b'\n').collect::<Vec<_>>();
        payload_lines.sort_unstable();
        expected_lines.sort_unstable();
        if payload_lines != expected_lines {
            return Err(format!("copied payload={payload:?}").into());
        }
        Ok(())
    }

    async fn live(
        &self,
        id: &str,
        command: &str,
        destination: &str,
        entries: &[(&str, &[u8])],
        expected: &[&str],
    ) -> Result<(), Error> {
        let name = self.create(id, command).await?;
        self.ready().await?;
        self.extract(&name, destination, entries).await?;
        let status =
            tokio::time::timeout(Duration::from_secs(45), self.containers.wait(&name)).await??;
        let output = String::from_utf8(self.containers.logs(&name).await?.stdout)?;
        if status != ExitStatus::Code(0)
            || expected
                .iter()
                .any(|line| !output.lines().any(|got| got == *line))
        {
            return Err(format!("status={status:?} stdout={output:?}").into());
        }
        Ok(())
    }

    async fn ready(&self) -> Result<(), Error> {
        let path = self.rootfs.join("tmp/cp-ready");
        tokio::time::timeout(Duration::from_secs(5), async {
            while !path.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        std::fs::remove_file(path)?;
        Ok(())
    }

    async fn extract(
        &self,
        name: &str,
        destination: &str,
        entries: &[(&str, &[u8])],
    ) -> Result<(), Error> {
        self.containers.filesystem(name).await?.extract(
            destination,
            Cursor::new(Archive::files(entries).0),
            Limits::default(),
        )?;
        Ok(())
    }

    async fn archive(&self, name: &str, path: &str) -> Result<Vec<u8>, Error> {
        let mut bytes = Vec::new();
        self.containers
            .filesystem(name)
            .await?
            .archive(path, &mut bytes)?;
        let mut payload = Vec::new();
        for item in tar::Archive::new(Cursor::new(bytes)).entries()? {
            let mut item = item?;
            if item.header().entry_type().is_file() {
                std::io::Read::read_to_end(&mut item, &mut payload)?;
            }
        }
        Ok(payload)
    }
}

struct Archive(Vec<u8>);

impl Archive {
    fn files(entries: &[(&str, &[u8])]) -> Self {
        let mut bytes = Vec::new();
        {
            let mut archive = tar::Builder::new(&mut bytes);
            for (path, contents) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_size(contents.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                archive.append_data(&mut header, path, *contents).unwrap();
            }
            archive.finish().unwrap();
        }
        Self(bytes)
    }
}
