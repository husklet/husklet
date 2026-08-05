//! Raw Docker create, list, and inspect projection of durable process and lifecycle state.

use crate::api::support::{raw_http, require, wait_for_path, write_named_image_archive};
use hl_container::{Config, Containers};
use hl_daemon::Daemon;
use hl_images::{
    Images,
    format::docker::{Archive, Limits},
};
use serde_json::{Value, json};
use std::path::Path;
use tempfile::TempDir;
use tokio::sync::oneshot;

pub(crate) async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    let archive = work.path().join("process.tar");
    write_named_image_archive(&archive, "scenario/process:v1", b"fixture\n")?;
    let images = Images::open(work.path().join("images"))?;
    Archive::load(std::fs::File::open(&archive)?, &images, Limits::default())?;
    let containers = Containers::builder(Config::new(work.path().join("state")))
        .images(images)
        .build()
        .await?;
    let socket = work.path().join("daemon.sock");
    let (shutdown, stopped) = oneshot::channel();
    let server = tokio::spawn(Daemon::new(containers).server(&socket).serve_with_shutdown(async move {
        let _ = stopped.await;
    }));
    wait_for_path(&socket).await?;

    let bind_source = work.path().join("bind-source");
    std::fs::create_dir(&bind_source)?;
    let result = exercise(&socket, &bind_source).await;
    let _ = remove(&socket).await;
    let _ = shutdown.send(());
    server.await??;
    result
}

async fn exercise(socket: &Path, bind_source: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let request = json!({
        "Image": "scenario/process:v1",
        "Entrypoint": ["/bin/process"],
        "Cmd": ["alpha", "two words"],
        "Labels": {"contract": "projection"},
        "ExposedPorts": {"80/tcp": {}},
        "HostConfig": {
            "PortBindings": {"80/tcp": [{"HostIp": "127.0.0.1", "HostPort": "0"}]},
            "Mounts": [
            {
                "Type": "bind",
                "Source": bind_source,
                "Target": "/bind",
                "ReadOnly": true,
                "BindOptions": {"Propagation": "private"}
            },
            {"Type": "volume", "Source": "projected-data", "Target": "/data"},
            {"Type": "tmpfs", "Target": "/scratch"}
        ]}
    });
    let created = exchange(
        socket,
        "POST",
        "/v1.43/containers/create?name=truthful-process",
        Some(serde_json::to_vec(&request)?),
    )
    .await?;
    require(
        created.0.starts_with("HTTP/1.1 201"),
        "container create was not HTTP 201",
    )?;
    let id = created.1["Id"].as_str().ok_or("container create omitted Id")?;
    let restart_probe = exchange(
        socket,
        "POST",
        "/v1.43/containers/create?name=restart-signal-probe",
        Some(serde_json::to_vec(&request)?),
    )
    .await?;
    require(
        restart_probe.0.starts_with("HTTP/1.1 201"),
        "restart signal probe create was not HTTP 201",
    )?;
    let restart_id = restart_probe.1["Id"]
        .as_str()
        .ok_or("restart signal probe omitted Id")?;

    let legacy_stop = exchange(
        socket,
        "POST",
        &format!("/v1.41/containers/{id}/stop?signal=KILL"),
        None,
    )
    .await?;
    require(
        legacy_stop.0.starts_with("HTTP/1.1 204"),
        "legacy container stop did not ignore the newer signal query",
    )?;
    let malformed_stop = exchange(
        socket,
        "POST",
        &format!("/v1.43/containers/{id}/stop?signal=SIGBOGUS"),
        None,
    )
    .await?;
    require(
        malformed_stop.0.starts_with("HTTP/1.1 204"),
        "inactive container stop validated an unused malformed signal",
    )?;
    let unsupported_stop = exchange(
        socket,
        "POST",
        &format!("/v1.43/containers/{id}/stop?signal=KILL"),
        None,
    )
    .await?;
    require(
        unsupported_stop.0.starts_with("HTTP/1.1 204"),
        "inactive container stop validated an unused signal override",
    )?;
    for target in [
        format!("/v1.24/containers/{id}/stop?t="),
        format!("/v1.43/containers/{id}/stop?t="),
        format!("/containers/{id}/stop?t=86401"),
    ] {
        let response = exchange(socket, "POST", &target, None).await?;
        require(
            response.0.starts_with("HTTP/1.1 204"),
            "inactive container stop rejected a Docker timeout form",
        )?;
    }
    let inactive_restart = exchange(
        socket,
        "POST",
        &format!("/v1.42/containers/{restart_id}/restart?signal=SIGBOGUS"),
        None,
    )
    .await?;
    require(
        !inactive_restart.0.starts_with("HTTP/1.1 400") && !inactive_restart.0.starts_with("HTTP/1.1 501"),
        "inactive container restart validated an unused malformed signal",
    )?;
    let unsupported_restart = exchange(
        socket,
        "POST",
        &format!("/containers/{restart_id}/restart?signal=KILL"),
        None,
    )
    .await?;
    require(
        unsupported_restart.0.starts_with("HTTP/1.1 501"),
        "active container restart silently ignored a custom signal override",
    )?;
    let stopped_probe = exchange(
        socket,
        "POST",
        &format!("/v1.43/containers/{restart_id}/stop?signal="),
        None,
    )
    .await?;
    require(
        stopped_probe.0.starts_with("HTTP/1.1 204"),
        "restart signal probe stop was not HTTP 204",
    )?;
    let removed_probe = exchange(
        socket,
        "DELETE",
        &format!("/v1.43/containers/{restart_id}?force=true"),
        None,
    )
    .await?;
    require(
        removed_probe.0.starts_with("HTTP/1.1 204"),
        "restart signal probe cleanup was not HTTP 204",
    )?;
    for target in [
        "/v1.42/containers/missing/stop?signal=SIGBOGUS",
        "/v1.43/containers/missing/restart?signal=KILL",
        "/v1.24/containers/missing/stop?t=",
        "/containers/missing/restart?t=",
    ] {
        let response = exchange(socket, "POST", target, None).await?;
        require(
            response.0.starts_with("HTTP/1.1 404"),
            "missing container did not take precedence over signal validation",
        )?;
    }
    let empty_stop = exchange(socket, "POST", &format!("/v1.42/containers/{id}/stop?signal="), None).await?;
    require(
        empty_stop.0.starts_with("HTTP/1.1 204"),
        "container stop rejected an empty signal query",
    )?;
    let configured_stop = exchange(
        socket,
        "POST",
        &format!("/v1.43/containers/{id}/stop?signal=TERM"),
        None,
    )
    .await?;
    require(
        configured_stop.0.starts_with("HTTP/1.1 204"),
        "container stop rejected its configured signal",
    )?;

    let websocket = exchange(
        socket,
        "GET",
        &format!("/v1.43/containers/{id}/attach/ws?stream=1&stdout=1"),
        None,
    )
    .await?;
    require(
        websocket.0.starts_with("HTTP/1.1 501"),
        "WebSocket attach capability was not truthfully refused",
    )?;
    require(
        websocket.1["message"]
            == "WebSocket container attach is not implemented; use the Docker raw-stream attach endpoint",
        "WebSocket attach refusal did not identify the supported alternative",
    )?;
    for target in [
        format!("/containers/{id}/attach/ws?stdout=1"),
        format!("/v1.24/containers/{id}/attach/ws?stdout=1"),
        format!("/v1.43/containers/{id}/attach/ws?unknown=ignored"),
    ] {
        let response = exchange(socket, "GET", &target, None).await?;
        require(
            response.0.starts_with("HTTP/1.1 501"),
            "WebSocket attach route was inconsistent",
        )?;
    }
    let upgraded = raw_http(
        socket,
        format!(
            "GET /v1.43/containers/{id}/attach/ws?stdin=1&stdout=1 HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade, close\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
        )
        .as_bytes(),
    )
    .await?;
    require(
        upgraded.starts_with("HTTP/1.1 501") && upgraded.contains("raw-stream attach endpoint"),
        "upgraded WebSocket attach was not truthfully refused",
    )?;
    for (target, status) in [
        (format!("/v1.43/containers/{id}/attach/ws?detachKeys=ctrl-!"), "400"),
        ("/v1.43/containers/missing/attach/ws?detachKeys=ctrl-!".into(), "400"),
        ("/v1.43/containers/missing/attach/ws?stdout=1".into(), "404"),
    ] {
        let response = exchange(socket, "GET", &target, None).await?;
        require(
            response.0.starts_with(&format!("HTTP/1.1 {status}")),
            "WebSocket attach validation ordering changed",
        )?;
    }
    for (method, suffix, status) in [("POST", "/attach/ws", "405"), ("GET", "/attach/ws/extra", "404")] {
        let response = raw_http(
            socket,
            format!(
                "{method} /v1.43/containers/{id}{suffix} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await?;
        require(
            response.starts_with(&format!("HTTP/1.1 {status}")),
            "WebSocket attach method or suffix isolation changed",
        )?;
    }

    let listed = exchange(socket, "GET", "/v1.43/containers/json?all=true", None).await?;
    require(listed.0.starts_with("HTTP/1.1 200"), "container list was not HTTP 200")?;
    let summaries = listed.1.as_array().ok_or("container list was not an array")?;
    require(
        summaries.len() == 1,
        "container list did not return the created container",
    )?;
    for (filters, expected) in [
        (r#"{"is-task":["false"]}"#, 1),
        (r#"{"is-task":{"false":false}}"#, 1),
        (r#"{"is-task":["true"]}"#, 0),
        (r#"{"is-task":["false","0"],"name":["truthful"]}"#, 1),
        (r#"{"is-task":["false"],"name":["missing"]}"#, 0),
        (r#"{"isolation":{"process":false}}"#, 1),
        (r#"{"isolation":["arbitrary"],"name":["missing"]}"#, 0),
    ] {
        let encoded = filters.bytes().fold(String::new(), |mut encoded, byte| {
            use std::fmt::Write as _;
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                encoded.push(char::from(byte));
            } else {
                write!(encoded, "%{byte:02X}").unwrap();
            }
            encoded
        });
        let response = exchange(
            socket,
            "GET",
            &format!("/v1.43/containers/json?all=true&filters={encoded}"),
            None,
        )
        .await?;
        require(response.0.starts_with("HTTP/1.1 200"), "is-task filter request failed")?;
        require(
            response.1.as_array().is_some_and(|values| values.len() == expected),
            "is-task filter selected the wrong ordinary-container set",
        )?;
    }
    let malformed = exchange(
        socket,
        "GET",
        "/v1.43/containers/json?all=true&filters=%7B%22is-task%22%3A%5B%22yes%22%5D%7D",
        None,
    )
    .await?;
    require(
        malformed.0.starts_with("HTTP/1.1 400"),
        "malformed is-task filter was accepted",
    )?;
    let conflicting = exchange(
        socket,
        "GET",
        "/v1.43/containers/json?all=true&filters=%7B%22is-task%22%3A%5B%22true%22%2C%22false%22%5D%7D",
        None,
    )
    .await?;
    require(
        conflicting.0.starts_with("HTTP/1.1 400"),
        "conflicting is-task filters were accepted",
    )?;
    let invalid_health = exchange(
        socket,
        "GET",
        "/v1.43/containers/json?all=true&filters=%7B%22health%22%3A%5B%22bogus%22%5D%7D",
        None,
    )
    .await?;
    require(
        invalid_health.0.starts_with("HTTP/1.1 400"),
        "invalid health filter was accepted",
    )?;
    let summary = &summaries[0];
    require(summary["Id"] == id, "container list changed Id")?;
    require(
        summary["Names"] == json!(["/truthful-process"]),
        "container list changed Names",
    )?;
    require(
        summary["Command"] == "/bin/process alpha two words",
        "container list changed effective Command",
    )?;
    require(
        summary["State"] == "created",
        "container list fabricated lifecycle State",
    )?;
    require(
        summary["Status"] == "Created",
        "container list fabricated lifecycle Status",
    )?;

    for (filters, count) in [
        (r#"{"expose":["80"]}"#, 1),
        (r#"{"expose":{"79-80/tcp":false}}"#, 1),
        (r#"{"expose":["80/udp"]}"#, 0),
        (r#"{"expose":["81","80"],"name":["truthful"]}"#, 1),
        (r#"{"expose":["80"],"name":["missing"]}"#, 0),
        (r#"{"publish":["80"]}"#, 1),
        (r#"{"publish":{"79-80/tcp":false}}"#, 1),
        (r#"{"publish":["81"]}"#, 0),
        (r#"{"publish":["80/udp"]}"#, 0),
        (r#"{"publish":["81","80"],"name":["truthful"]}"#, 1),
        (r#"{"publish":["80"],"name":["missing"]}"#, 0),
    ] {
        let filtered = exchange(
            socket,
            "GET",
            &format!("/v1.43/containers/json?all=true&filters={}", percent_encode(filters)),
            None,
        )
        .await?;
        require(filtered.0.starts_with("HTTP/1.1 200"), "expose filter was not HTTP 200")?;
        require(
            filtered.1.as_array().is_some_and(|items| items.len() == count),
            "expose filter selected the wrong containers",
        )?;
    }
    let bind_source_filter = serde_json::to_string(&json!({
        "volume": [bind_source.to_string_lossy()]
    }))?;
    for (version, filters, count) in [
        ("v1.24", r#"{"volume":["projected-data"]}"#.to_owned(), 1),
        (
            "v1.43",
            r#"{"volume":{"missing":true,"projected-data":false}}"#.to_owned(),
            1,
        ),
        ("v1.43", bind_source_filter, 1),
        ("v1.43", r#"{"volume":["/bind"]}"#.to_owned(), 1),
        ("v1.43", r#"{"volume":["/data"]}"#.to_owned(), 1),
        ("v1.43", r#"{"volume":["/scratch"]}"#.to_owned(), 1),
        ("v1.43", r#"{"volume":[""]}"#.to_owned(), 1),
        (
            "v1.43",
            r#"{"volume":["projected-data"],"name":["missing"]}"#.to_owned(),
            0,
        ),
    ] {
        let filtered = exchange(
            socket,
            "GET",
            &format!(
                "/{version}/containers/json?all=true&filters={}",
                percent_encode(&filters)
            ),
            None,
        )
        .await?;
        require(filtered.0.starts_with("HTTP/1.1 200"), "volume filter was not HTTP 200")?;
        require(
            filtered.1.as_array().is_some_and(|items| items.len() == count),
            "volume filter selected the wrong containers",
        )?;
    }
    let malformed = exchange(
        socket,
        "GET",
        &format!(
            "/v1.43/containers/json?all=true&filters={}",
            percent_encode(r#"{"expose":["90-80"]}"#)
        ),
        None,
    )
    .await?;
    require(
        malformed.0.starts_with("HTTP/1.1 400"),
        "malformed expose filter was not rejected",
    )?;

    let inspected = exchange(socket, "GET", &format!("/v1.43/containers/{id}/json"), None).await?;
    require(
        inspected.0.starts_with("HTTP/1.1 200"),
        "container inspect was not HTTP 200",
    )?;
    require(inspected.1["Id"] == id, "container inspect changed Id")?;
    require(
        inspected.1["Name"] == "/truthful-process",
        "container inspect changed Name",
    )?;
    require(
        inspected.1["Path"] == "/bin/process",
        "container inspect omitted effective Path",
    )?;
    require(
        inspected.1["Args"] == json!(["alpha", "two words"]),
        "container inspect omitted effective Args",
    )?;
    let state = &inspected.1["State"];
    require(state["Status"] == "created", "inspect State.Status was not created")?;
    require(state["Running"] == false, "inspect State.Running was fabricated")?;
    require(state["Paused"] == false, "inspect State.Paused was fabricated")?;
    require(state["Restarting"] == false, "inspect State.Restarting was fabricated")?;
    require(state["Pid"] == 0, "inspect State.Pid was fabricated")?;
    require(state["ExitCode"] == 0, "inspect State.ExitCode was fabricated")?;

    let mounts = &summary["Mounts"];
    require(
        mounts == &inspected.1["Mounts"],
        "container list and inspect projected different mounts",
    )?;
    let mounts = mounts.as_array().ok_or("container Mounts was not an array")?;
    require(mounts.len() == 3, "container omitted a configured mount")?;
    require(
        mounts[0]
            == json!({
                "Type": "bind", "Name": "", "Source": bind_source,
                "Destination": "/bind", "Driver": "", "Mode": "ro",
                "RW": false, "Propagation": "private"
            }),
        "bind mount projection changed durable fields",
    )?;
    require(mounts[1]["Type"] == "volume", "managed mount type changed")?;
    require(mounts[1]["Name"] == "projected-data", "managed mount name changed")?;
    require(mounts[1]["Driver"] == "local", "managed mount driver changed")?;
    require(mounts[1]["Destination"] == "/data", "managed mount target changed")?;
    require(mounts[1]["Mode"] == "rw", "managed mount mode changed")?;
    require(mounts[1]["RW"] == true, "managed mount access changed")?;
    require(mounts[1]["Propagation"] == "", "managed mount invented propagation")?;
    require(
        mounts[1]["Source"]
            .as_str()
            .is_some_and(|source| source.ends_with("/_data")),
        "managed mount omitted its resolved source",
    )?;
    require(
        mounts[2]
            == json!({
                "Type": "tmpfs", "Name": "", "Source": "",
                "Destination": "/scratch", "Driver": "", "Mode": "",
                "RW": true, "Propagation": ""
            }),
        "tmpfs mount leaked internal backing state",
    )?;

    let sized = exchange(socket, "GET", "/v1.43/containers/json?all=true&size=true", None).await?;
    require(
        sized.0.starts_with("HTTP/1.1 200"),
        "sized container list was not HTTP 200",
    )?;
    require(
        sized.1[0]["Mounts"] == inspected.1["Mounts"],
        "sized container list projected different mounts",
    )
}

fn percent_encode(value: &str) -> String {
    use std::fmt::Write as _;

    value.bytes().fold(String::new(), |mut encoded, byte| {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            write!(encoded, "%{byte:02X}").expect("writing to a string cannot fail");
        }
        encoded
    })
}

async fn remove(socket: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let response = exchange(
        socket,
        "DELETE",
        "/v1.43/containers/truthful-process?force=true&v=true",
        None,
    )
    .await?;
    require(
        response.0.starts_with("HTTP/1.1 204"),
        "container cleanup was not HTTP 204",
    )
}

async fn exchange(
    socket: &Path,
    method: &str,
    target: &str,
    body: Option<Vec<u8>>,
) -> Result<(String, Value), Box<dyn std::error::Error>> {
    let body = body.unwrap_or_default();
    let request = format!(
        "{method} {target} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let mut request = request.into_bytes();
    request.extend_from_slice(&body);
    let response = raw_http(socket, &request).await?;
    let (head, body) = response
        .split_once("\r\n\r\n")
        .ok_or("container response omitted body separator")?;
    let value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(body)?
    };
    Ok((head.to_owned(), value))
}
