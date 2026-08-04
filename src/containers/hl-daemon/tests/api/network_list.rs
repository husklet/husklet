//! Docker network-list filter contracts over the raw daemon wire.

use crate::api::support::{raw_http, require, wait_for_path};
use hl_container::{Config, Containers, NetworkSpec, Subnet};
use hl_daemon::Daemon;
use serde_json::Value;
use tempfile::TempDir;
use tokio::sync::oneshot;

pub(crate) async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    let containers = Containers::builder(Config::new(work.path().join("state")))
        .build()
        .await?;
    let alpha = containers
        .networks()
        .create(
            NetworkSpec::bridge("alpha-prod", Subnet::new("10.91.0.0".parse()?, 24)?)
                .label("tier", "prod")
                .label("stage", "blue"),
        )
        .await?;
    containers
        .networks()
        .create(
            NetworkSpec::bridge("beta-dev", Subnet::new("10.92.0.0".parse()?, 24)?)
                .label("tier", "prod")
                .label("stage", "red"),
        )
        .await?;

    let socket = work.path().join("daemon.sock");
    let (shutdown, stopped) = oneshot::channel();
    let server = tokio::spawn(Daemon::new(containers).server(&socket).serve_with_shutdown(async move {
        let _ = stopped.await;
    }));
    wait_for_path(&socket).await?;

    assert_names(
        &socket,
        r#"{"driver":{"bridge":true,"null":false}}"#,
        &["alpha-prod", "beta-dev", "bridge", "none"],
    )
    .await?;
    assert_names(
        &socket,
        r#"{"driver":["bridge","null"],"label":["tier=prod","stage=blue"]}"#,
        &["alpha-prod"],
    )
    .await?;
    assert_names(&socket, r#"{"name":["^alpha-"]}"#, &["alpha-prod"]).await?;
    assert_names(&socket, r#"{"name":["prod"]}"#, &["alpha-prod"]).await?;
    let suffix = &alpha.id.as_str()[alpha.id.as_str().len() - 8..];
    assert_names(&socket, &format!(r#"{{"id":["{suffix}$"]}}"#), &["alpha-prod"]).await?;
    assert_names(&socket, r#"{"label":["tier","stage=red"]}"#, &["beta-dev"]).await?;
    assert_names(
        &socket,
        r#"{"scope":["swarm","local"]}"#,
        &["alpha-prod", "beta-dev", "bridge", "none"],
    )
    .await?;
    assert_names(&socket, r#"{"type":["builtin"]}"#, &["bridge", "none"]).await?;
    assert_names(
        &socket,
        r#"{"type":["builtin","custom"]}"#,
        &["alpha-prod", "beta-dev", "bridge", "none"],
    )
    .await?;
    assert_names(&socket, r#"{"dangling":["true"]}"#, &["alpha-prod", "beta-dev"]).await?;
    assert_names(&socket, r#"{"dangling":{"1":true}}"#, &["alpha-prod", "beta-dev"]).await?;
    assert_names(&socket, r#"{"dangling":["false"]}"#, &["bridge", "none"]).await?;
    assert_names(&socket, r#"{"name":["["]}"#, &[]).await?;

    for invalid in [
        r#"{"until":["1h"]}"#,
        r#"{"dangling":["sometimes"]}"#,
        r#"{"dangling":["true","false"]}"#,
        r#"{"name":[1]}"#,
        r#"{"scope":{"local":"yes"}}"#,
    ] {
        let response = exchange(&socket, invalid).await?;
        require(
            response.starts_with("HTTP/1.1 400"),
            "invalid network filter was not HTTP 400",
        )?;
    }
    let invalid_type = exchange(&socket, r#"{"type":["overlay"]}"#).await?;
    require(
        invalid_type.starts_with("HTTP/1.1 500"),
        "invalid network type did not preserve Moby's HTTP 500",
    )?;

    let _ = shutdown.send(());
    server.await??;
    println!("PASS network-list-filters");
    Ok(())
}

async fn assert_names(
    socket: &std::path::Path,
    filters: &str,
    expected: &[&str],
) -> Result<(), Box<dyn std::error::Error>> {
    let response = exchange(socket, filters).await?;
    require(response.starts_with("HTTP/1.1 200"), "network list was not HTTP 200")?;
    let (_, body) = response
        .split_once("\r\n\r\n")
        .ok_or("HTTP response omitted body separator")?;
    let names = serde_json::from_str::<Vec<Value>>(body)?
        .into_iter()
        .map(|network| {
            network
                .get("Name")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or("network response omitted Name")
        })
        .collect::<Result<Vec<_>, _>>()?;
    let expected = expected.iter().map(|value| (*value).to_owned()).collect::<Vec<_>>();
    require(names == expected, "network filter returned unexpected names")
}

async fn exchange(socket: &std::path::Path, filters: &str) -> Result<String, Box<dyn std::error::Error>> {
    let encoded = filters.bytes().map(|byte| format!("%{byte:02X}")).collect::<String>();
    let request =
        format!("GET /v1.43/networks?filters={encoded} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    raw_http(socket, request.as_bytes()).await
}
