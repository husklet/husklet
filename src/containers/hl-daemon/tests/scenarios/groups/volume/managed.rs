use std::path::Path;

use crate::report::ScenarioBatch;
use hl_client::{
    model::{CreateContainer, VolumeCreate},
    Client,
};

use super::{
    execution::{execute, pass, request},
    Error, IMAGE,
};

pub(super) async fn run_docker_contracts(client: &Client) -> Result<(), Error> {
    let dockers = crate::registry::dockervol::group()
        .scenarios
        .into_iter()
        .map(|value| (value.id, value))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut reports = ScenarioBatch::new("dockervol")?;
    let managed = Managed::new(client);
    for (ids, kind) in managed_groups() {
        let mut attempts = Vec::new();
        for id in ids {
            let scenario = &dockers[id];
            if let Some(attempt) = reports.begin(scenario)? {
                attempts.push((scenario, attempt));
            }
        }
        if attempts.is_empty() {
            continue;
        }
        let result = match kind {
            0 => managed.named().await,
            1 => managed.anonymous().await,
            2 => managed.temporary().await,
            3 => managed.subpaths().await,
            4 => managed.advanced_binds().await,
            5 => managed.local_driver().await,
            _ => unreachable!(),
        };
        for (scenario, attempt) in attempts {
            reports.complete(scenario, attempt, &result)?;
        }
        result?;
    }
    reports.finish(Vec::new())?;
    Ok(())
}

fn managed_groups() -> [(&'static [&'static str], u8); 6] {
    [
        (
            &[
                "dockervol/create-ls",
                "dockervol/inspect",
                "dockervol/persist-across-runs",
                "dockervol/mount-volume-inuse",
                "dockervol/rm",
            ],
            0,
        ),
        (&["dockervol/anon-volume"], 1),
        (&["dockervol/tmpfs-fresh", "dockervol/mount-tmpfs"], 2),
        (
            &[
                "dockervol/subpath",
                "dockervol/subpath-missing",
                "dockervol/subpath-symlink-escape",
            ],
            3,
        ),
        (
            &[
                "dockervol/bind-private-recursive-ro",
                "dockervol/bind-shared-reject",
                "dockervol/bind-nonrecursive-reject",
            ],
            4,
        ),
        (
            &[
                "dockervol/local-bind",
                "dockervol/local-bind-inspect",
                "dockervol/local-filesystem-reject",
            ],
            5,
        ),
    ]
}

struct Managed<'a> {
    client: &'a Client,
}

impl<'a> Managed<'a> {
    fn new(client: &'a Client) -> Self {
        Self { client }
    }

    async fn local_driver(&self) -> Result<(), Error> {
        let device = tempfile::tempdir()?;
        std::fs::write(device.path().join("value"), b"LOCAL_BIND_OK\n")?;
        let volumes = self.client.volumes();
        let name = "contract-local-bind";
        let volume = volumes
            .create(&VolumeCreate {
                name: name.into(),
                driver: "local".into(),
                driver_opts: std::collections::BTreeMap::from([
                    ("type".into(), "none".into()),
                    ("o".into(), "bind,ro".into()),
                    ("device".into(), device.path().display().to_string()),
                ]),
                ..VolumeCreate::default()
            })
            .await?;
        let output = execute(
            self.client,
            "local-bind-volume",
            "cat /data/value",
            vec![format!("{name}:/data")],
        )
        .await?;
        pass(output == b"LOCAL_BIND_OK\n", "dockervol/local-bind")?;
        let inspected = volumes.inspect(name).await?;
        pass(
            volume.driver == "local"
                && inspected.scope == "local"
                && inspected
                    .options
                    .get("type")
                    .is_some_and(|value| value == "none")
                && inspected
                    .options
                    .get("o")
                    .is_some_and(|value| value == "bind,ro"),
            "dockervol/local-bind-inspect",
        )?;
        volumes.remove(name, false).await?;
        let rejected = volumes
            .create(&VolumeCreate {
                name: "contract-local-invalid".into(),
                driver: "local".into(),
                driver_opts: std::collections::BTreeMap::from([
                    ("type".into(), "ext4".into()),
                    ("o".into(), "bind".into()),
                    ("device".into(), device.path().display().to_string()),
                ]),
                ..VolumeCreate::default()
            })
            .await;
        pass(rejected.is_err(), "dockervol/local-filesystem-reject")
    }

    async fn named(&self) -> Result<(), Error> {
        let volumes = self.client.volumes();
        let volume = volumes
            .create(&VolumeCreate {
                name: "contract-volume".into(),
                ..VolumeCreate::default()
            })
            .await?;
        pass(
            volumes
                .list()
                .await?
                .volumes
                .iter()
                .any(|item| item.name == volume.name),
            "dockervol/create-ls",
        )?;
        pass(
            volumes.inspect(&volume.name).await?.name == volume.name,
            "dockervol/inspect",
        )?;
        execute(
            self.client,
            "managed-write",
            "echo VOLPERSIST > /data/f",
            vec!["contract-volume:/data".into()],
        )
        .await?;
        let read = execute(
            self.client,
            "managed-read",
            "cat /data/f",
            vec!["contract-volume:/data".into()],
        )
        .await?;
        pass(read == b"VOLPERSIST\n", "dockervol/persist-across-runs")?;
        let held = self
            .client
            .containers()
            .create(
                &request("sleep 60", vec!["contract-volume:/data".into()]),
                Some("managed-held"),
            )
            .await?;
        let error = volumes
            .remove(&volume.name, false)
            .await
            .expect_err("in-use volume removed");
        let error = error.to_string().to_lowercase();
        pass(
            error.contains("in use") || error.contains("referenced") || error.contains("cannot"),
            "dockervol/mount-volume-inuse",
        )?;
        self.client
            .containers()
            .remove(&held.id, true, false)
            .await?;
        volumes.remove(&volume.name, false).await?;
        pass(
            !volumes
                .list()
                .await?
                .volumes
                .iter()
                .any(|item| item.name == volume.name),
            "dockervol/rm",
        )?;
        Ok(())
    }

    async fn anonymous(&self) -> Result<(), Error> {
        let anonymous: CreateContainer = serde_json::from_value(serde_json::json!({
            "Image": IMAGE, "Cmd": ["/bin/sh", "-c", "echo ANON > /data/f; cat /data/f"], "Volumes": {"/data": {}}
        }))?;
        let anonymous = self
            .client
            .containers()
            .create(&anonymous, Some("managed-anon"))
            .await?;
        self.client.containers().start(&anonymous.id).await?;
        pass(
            self.client
                .containers()
                .wait(&anonymous.id)
                .await?
                .status_code
                == 0,
            "dockervol/anon-exit",
        )?;
        let inspect = self.client.containers().inspect(&anonymous.id).await?;
        pass(
            self.client
                .containers()
                .logs(&anonymous.id, true, false)
                .await?
                .stdout
                == b"ANON\n"
                && inspect
                    .metadata
                    .mounts
                    .iter()
                    .any(|mount| mount.kind == "volume" && mount.destination == "/data"),
            "dockervol/anon-volume",
        )?;
        self.client
            .containers()
            .remove(&anonymous.id, false, true)
            .await?;
        Ok(())
    }

    async fn temporary(&self) -> Result<(), Error> {
        let first = tmpfs(
            self.client,
            "tmpfs-first",
            "/cache",
            "echo hi > /cache/f; cat /cache/f; ls -1 /cache | wc -l | tr -d ' '",
        )
        .await?;
        let second = tmpfs(
            self.client,
            "tmpfs-second",
            "/cache",
            "ls -1 /cache | wc -l | tr -d ' '",
        )
        .await?;
        pass(
            first.0 == b"hi\n1\n" && second.0 == b"0\n",
            "dockervol/tmpfs-fresh",
        )?;
        let mounted = tmpfs(
            self.client,
            "tmpfs-inspect",
            "/scratch",
            "echo ok > /scratch/f; cat /scratch/f",
        )
        .await?;
        pass(
            mounted.0 == b"ok\n"
                && mounted
                    .1
                    .metadata
                    .mounts
                    .iter()
                    .any(|mount| mount.kind == "tmpfs" && mount.destination == "/scratch"),
            "dockervol/mount-tmpfs",
        )?;
        Ok(())
    }

    async fn subpaths(&self) -> Result<(), Error> {
        let volume = self
            .client
            .volumes()
            .create(&VolumeCreate {
                name: "subpath-volume".into(),
                ..Default::default()
            })
            .await?;
        std::fs::create_dir_all(Path::new(&volume.mountpoint).join("safe"))?;
        std::fs::write(
            Path::new(&volume.mountpoint).join("safe/value"),
            b"SUBPATH_OK\n",
        )?;
        std::os::unix::fs::symlink("/tmp", Path::new(&volume.mountpoint).join("escape"))?;
        let request = |subpath: &str| -> Result<CreateContainer, Error> {
            Ok(serde_json::from_value(serde_json::json!({
                "Image": IMAGE,
                "Cmd": ["cat", "/data/value"],
                "HostConfig": {"Mounts": [{"Type": "volume", "Source": "subpath-volume", "Target": "/data", "ReadOnly": true, "VolumeOptions": {"Subpath": subpath}}]}
            }))?)
        };
        let valid = self
            .client
            .containers()
            .create(&request("safe")?, Some("subpath-valid"))
            .await?;
        self.client.containers().start(&valid.id).await?;
        let status = self.client.containers().wait(&valid.id).await?;
        let logs = self.client.containers().logs(&valid.id, true, true).await?;
        pass(
            status.status_code == 0 && logs.stdout == b"SUBPATH_OK\n",
            "dockervol/subpath",
        )?;
        for (id, path) in [
            ("dockervol/subpath-missing", "missing"),
            ("dockervol/subpath-symlink-escape", "escape"),
        ] {
            let created = self
                .client
                .containers()
                .create(&request(path)?, Some(&id.replace('/', "-")))
                .await?;
            pass(
                self.client.containers().start(&created.id).await.is_err(),
                id,
            )?;
        }
        Ok(())
    }

    async fn advanced_binds(&self) -> Result<(), Error> {
        let volume = self
            .client
            .volumes()
            .create(&VolumeCreate {
                name: "bind-options-source".into(),
                ..Default::default()
            })
            .await?;
        std::fs::create_dir_all(Path::new(&volume.mountpoint).join("nested"))?;
        let request = |propagation: &str, non_recursive: bool| -> Result<CreateContainer, Error> {
            Ok(serde_json::from_value(serde_json::json!({
                "Image": IMAGE,
                "Cmd": ["/bin/sh", "-c", "echo x > /data/nested/x 2>/dev/null || echo RECURSIVE_RO_OK"],
                "HostConfig": {"Mounts": [{"Type": "bind", "Source": volume.mountpoint, "Target": "/data", "ReadOnly": true, "BindOptions": {"Propagation": propagation, "NonRecursive": non_recursive, "ReadOnlyForceRecursive": true}}]}
            }))?)
        };
        let valid = self
            .client
            .containers()
            .create(&request("private", false)?, Some("advanced-bind-valid"))
            .await?;
        self.client.containers().start(&valid.id).await?;
        let status = self.client.containers().wait(&valid.id).await?;
        let logs = self.client.containers().logs(&valid.id, true, true).await?;
        let inspect = self.client.containers().inspect(&valid.id).await?;
        pass(
            status.status_code == 0
                && logs.stdout == b"RECURSIVE_RO_OK\n"
                && inspect.metadata.mounts[0].propagation == "private",
            "dockervol/bind-private-recursive-ro",
        )?;
        for (id, propagation, non_recursive) in [
            ("dockervol/bind-shared-reject", "rshared", false),
            ("dockervol/bind-nonrecursive-reject", "rprivate", true),
        ] {
            pass(
                self.client
                    .containers()
                    .create(
                        &request(propagation, non_recursive)?,
                        Some(&id.replace('/', "-")),
                    )
                    .await
                    .is_err(),
                id,
            )?;
        }
        Ok(())
    }
}

async fn tmpfs(
    client: &Client,
    name: &str,
    target: &str,
    command: &str,
) -> Result<(Vec<u8>, hl_client::model::InspectContainer), Error> {
    let request: CreateContainer = serde_json::from_value(serde_json::json!({
        "Image": IMAGE,
        "Cmd": ["/bin/sh", "-c", command],
        "HostConfig": {"Mounts": [{"Type": "tmpfs", "Target": target}]}
    }))?;
    let created = client.containers().create(&request, Some(name)).await?;
    client.containers().start(&created.id).await?;
    let status = client.containers().wait(&created.id).await?;
    let logs = client.containers().logs(&created.id, true, true).await?;
    let inspect = client.containers().inspect(&created.id).await?;
    client.containers().remove(&created.id, false, true).await?;
    if status.status_code != 0 {
        return Err(format!("{name} exited {}", status.status_code).into());
    }
    Ok((logs.stdout, inspect))
}
