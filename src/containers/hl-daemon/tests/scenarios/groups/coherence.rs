//! Filesystem coherence compatibility cases.

use hl_container::{ContainerSpec, Containers, Isolation, Limits, Process, Sandbox};
use std::{io::Cursor, path::Path, time::Duration};

type Error = Box<dyn std::error::Error>;

const IDS: [&str; 8] = [
    "cpcoherence/cp-dir-tree-live-poll",
    "cpcoherence/cp-dir-tree-live-poll.amd",
    "cpcoherence/cp-into-held-open-dir",
    "cpcoherence/cp-into-held-open-dir.amd",
    "cpcoherence/cp-new-file-live-poll",
    "cpcoherence/cp-new-file-live-poll.amd",
    "cpcoherence/cp-overwrite-cached-positive",
    "cpcoherence/cp-overwrite-cached-positive.amd",
];

pub(crate) fn group() -> crate::contract::Group {
    use crate::contract::{Scenario, Target};
    crate::contract::Group::new(
        "cpcoherence",
        IDS.into_iter()
            .map(|id| {
                let target = if id.strip_suffix(".amd").is_some() {
                    Target::Amd64
                } else {
                    Target::Arm64
                };
                Scenario::new(id, "alpine:3.20").api(id).only(&[target])
            })
            .collect(),
    )
}

pub(crate) async fn run(containers: &Containers, rootfs: &Path) -> Result<(), Error> {
    for case in Case::all() {
        case.run(containers, rootfs).await?;
    }
    Ok(())
}

struct Case {
    name: &'static str,
    command: &'static str,
    destination: &'static str,
    ready: &'static str,
    archive: Archive,
    expected: &'static [&'static str],
}

impl Case {
    fn all() -> [Self; 4] {
        [
            Self {
                name: "copy-new",
                command: "d=/tmp/hl-coherence/copy-new; mkdir -p $d; touch $d/.ready; i=0; while [ $i -lt 200 ]; do if [ -e $d/probe ]; then echo SEEN:$(cat $d/probe); exit 0; fi; i=$((i+1)); sleep .05; done; exit 1",
                destination: "/tmp/hl-coherence/copy-new",
                ready: "/tmp/hl-coherence/copy-new/.ready",
                archive: Archive::files(&[("probe", b"hello-cp\n")]),
                expected: &["SEEN:hello-cp"],
            },
            Self {
                name: "copy-overwrite",
                command: "d=/tmp/hl-coherence/copy-overwrite; mkdir -p $d; : > $d/probe; touch $d/.ready; i=0; while [ $i -lt 200 ]; do if [ -s $d/probe ]; then echo GREW:$(cat $d/probe); exit 0; fi; i=$((i+1)); sleep .05; done; exit 1",
                destination: "/tmp/hl-coherence/copy-overwrite",
                ready: "/tmp/hl-coherence/copy-overwrite/.ready",
                archive: Archive::files(&[("probe", b"new-content\n")]),
                expected: &["GREW:new-content"],
            },
            Self {
                name: "copy-tree",
                command: "d=/tmp/hl-coherence/copy-tree; mkdir -p $d; touch $d/.ready; i=0; while [ $i -lt 200 ]; do if [ -e $d/sub/leaf ]; then echo TREE:$(cat $d/sub/leaf); exit 0; fi; i=$((i+1)); sleep .05; done; exit 1",
                destination: "/tmp/hl-coherence/copy-tree",
                ready: "/tmp/hl-coherence/copy-tree/.ready",
                archive: Archive::files(&[("sub/leaf", b"leaf-content\n")]),
                expected: &["TREE:leaf-content"],
            },
            Self {
                name: "copy-held",
                command: "d=/tmp/hl-coherence/copy-held; mkdir -p $d/held; cd $d/held; touch ../.ready; i=0; while [ $i -lt 200 ]; do if [ -e ./probe ]; then echo HELD:$(cat ./probe); echo LIST:$(ls); exit 0; fi; i=$((i+1)); sleep .05; done; exit 1",
                destination: "/tmp/hl-coherence/copy-held/held",
                ready: "/tmp/hl-coherence/copy-held/.ready",
                archive: Archive::files(&[("probe", b"held-content\n")]),
                expected: &["HELD:held-content", "LIST:probe"],
            },
        ]
    }

    async fn run(self, containers: &Containers, rootfs: &Path) -> Result<(), Error> {
        let host = rootfs.join("tmp/hl-coherence").join(self.name);
        std::fs::create_dir_all(rootfs.join("tmp/hl-coherence"))?;
        let _ = std::fs::remove_dir_all(&host);
        let container = containers
            .create(
                ContainerSpec::from_directory(
                    rootfs,
                    Process::new("/bin/sh").args(["-c", self.command]),
                )
                .name(self.name)
                .isolation(Isolation {
                    sandbox: Sandbox::Disabled,
                    ..Isolation::default()
                }),
            )
            .await?;
        containers.start(container.id.as_str()).await?;
        let result = self.copy(containers, rootfs, container.id.as_str()).await;
        if result.is_err() {
            let _ = containers.stop(container.id.as_str(), Duration::ZERO).await;
        }
        result
    }

    async fn copy(&self, containers: &Containers, rootfs: &Path, id: &str) -> Result<(), Error> {
        let ready = rootfs.join(self.ready.trim_start_matches('/'));
        tokio::time::timeout(Duration::from_secs(5), async {
            while !ready.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        containers.filesystem(id).await?.extract(
            self.destination,
            Cursor::new(&self.archive.0),
            Limits::default(),
        )?;
        let exit = tokio::time::timeout(Duration::from_secs(12), containers.wait(id)).await??;
        if exit != hl_container::ExitStatus::Code(0) {
            return Err(format!("{} exited with {exit:?}", self.name).into());
        }
        let output = String::from_utf8(containers.logs(id).await?.stdout)?;
        for expected in self.expected {
            if !output.lines().any(|line| line == *expected) {
                return Err(
                    format!("{} output omitted {expected:?}: {output:?}", self.name).into(),
                );
            }
        }
        Ok(())
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
