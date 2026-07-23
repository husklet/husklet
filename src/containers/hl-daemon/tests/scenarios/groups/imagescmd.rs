use crate::api::support::{raw_http, wait_for_path, write_image_archive};
use crate::report::LegacyBatch;
use hl_client::Client;
use hl_container::{Config, Containers};
use hl_daemon::Daemon;
use std::path::Path;
use tempfile::TempDir;
use tokio::sync::oneshot;

type Error = Box<dyn std::error::Error>;

struct Images {
    work: TempDir,
}

impl Images {
    fn new() -> Result<Self, Error> {
        Ok(Self {
            work: TempDir::new()?,
        })
    }

    async fn run(self) -> Result<(), Error> {
        let archive = self.work.path().join("fixture.tar");
        write_image_archive(&archive)?;
        let containers = Containers::builder(Config::new(self.work.path().join("state")))
            .build()
            .await?;
        let socket = self.work.path().join("daemon.sock");
        let (shutdown, stopped) = oneshot::channel();
        let server = tokio::spawn(Daemon::new(containers).server(&socket).serve_with_shutdown(
            async move {
                let _ = stopped.await;
            },
        ));
        wait_for_path(&socket).await?;
        let client = Client::unix(&socket)?;
        client
            .images()
            .load(tokio::fs::File::open(&archive).await?)
            .await?;
        client
            .images()
            .tag("scenario/fixture:test", "alpine", Some("latest"))
            .await?;

        let mut failures = Vec::new();
        let scenarios = crate::registry::images::group()
            .scenarios
            .into_iter()
            .map(|value| (value.id, value))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut reports = LegacyBatch::new("imagescmd")?;
        for id in [
            "imagescmd/list",
            "imagescmd/tag",
            "imagescmd/rmi",
            "imagescmd/history",
            "imagescmd/inspect",
        ] {
            let scenario = &scenarios[id];
            let Some(attempt) = reports.begin(scenario)? else {
                println!("RESUME {id}");
                continue;
            };
            let result = match id {
                "imagescmd/list" => self.list(&client).await,
                "imagescmd/tag" => self.tag(&client).await,
                "imagescmd/rmi" => self.remove(&client).await,
                "imagescmd/history" => self.history(&socket).await,
                "imagescmd/inspect" => self.inspect(&socket).await,
                _ => unreachable!(),
            };
            reports.complete(scenario, attempt, &result)?;
            match result {
                Ok(()) => println!("PASS {id}"),
                Err(error) => {
                    println!("FAIL {id}: {error}");
                    failures.push(format!("{id}: {error}"));
                }
            }
        }
        let _ = shutdown.send(());
        reports.finish(Vec::new())?;
        server.await??;
        println!(
            "image command scenarios: {} passed; {} failed; 5 total",
            5 - failures.len(),
            failures.len()
        );
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("\n").into())
        }
    }

    async fn list(&self, client: &Client) -> Result<(), Error> {
        let listed = client.images().list().await?;
        if listed.iter().any(|image| {
            image
                .repo_tags
                .iter()
                .any(|tag| tag == "docker.io/library/alpine:latest")
        }) {
            Ok(())
        } else {
            Err("alpine:latest was absent from image list".into())
        }
    }

    async fn tag(&self, client: &Client) -> Result<(), Error> {
        client
            .images()
            .tag("alpine:latest", "scenario/imagescmd", Some("v1"))
            .await?;
        let tagged = client.images().list().await?.iter().any(|image| {
            image
                .repo_tags
                .iter()
                .any(|tag| tag.ends_with("/scenario/imagescmd:v1"))
        });
        client.images().remove("scenario/imagescmd:v1").await?;
        if tagged {
            Ok(())
        } else {
            Err("new image tag was absent from image list".into())
        }
    }

    async fn remove(&self, client: &Client) -> Result<(), Error> {
        client
            .images()
            .tag("alpine:latest", "scenario/imagescmd", Some("remove"))
            .await?;
        client.images().remove("scenario/imagescmd:remove").await?;
        if client
            .images()
            .inspect("scenario/imagescmd:remove")
            .await
            .is_err()
        {
            Ok(())
        } else {
            Err("removed image tag remained inspectable".into())
        }
    }

    async fn history(&self, socket: &Path) -> Result<(), Error> {
        let response = raw_http(
            socket,
            b"GET /v1.43/images/alpine:latest/history HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )
        .await?;
        if response.starts_with("HTTP/1.1 200") {
            Ok(())
        } else {
            Err(format!(
                "history response: {}",
                response.lines().next().unwrap_or("empty")
            )
            .into())
        }
    }

    async fn inspect(&self, socket: &Path) -> Result<(), Error> {
        let response = raw_http(
            socket,
            b"GET /v1.43/images/alpine:latest/json HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )
        .await?;
        if response.contains("\"Os\":\"linux\"") && response.contains("\"Architecture\":") {
            Ok(())
        } else {
            Err("image inspect omitted Linux OS/architecture metadata".into())
        }
    }
}

pub(crate) async fn run() -> Result<(), Error> {
    Images::new()?.run().await
}
