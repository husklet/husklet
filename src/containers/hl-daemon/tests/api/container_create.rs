//! Table-driven Docker create validation, projection, and rollback contracts.

use crate::api::support::{raw_http, require, wait_for_path, write_named_image_archive};
use hl_container::{Config, Containers};
use hl_daemon::Daemon;
use hl_images::{
    Images,
    format::docker::{Archive, Limits},
};
use serde_json::{Map, Value, json};
use std::{collections::BTreeSet, path::Path};
use tempfile::TempDir;
use tokio::sync::oneshot;

type Error = Box<dyn std::error::Error>;

pub(crate) async fn run() -> Result<(), Error> {
    let work = TempDir::new()?;
    let archive = work.path().join("create.tar");
    write_named_image_archive(&archive, "contract/create:v1", b"fixture\n")?;
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

    let result = exercise(&socket).await;
    let _ = shutdown.send(());
    server.await??;
    result
}

async fn exercise(socket: &Path) -> Result<(), Error> {
    supported_and_projected(socket).await?;
    zero_compatibility_fields_are_inert(socket).await?;
    meaningful_unsupported_fields_are_refused(socket).await?;
    failed_create_releases_anonymous_volumes(socket).await
}

async fn supported_and_projected(socket: &Path) -> Result<(), Error> {
    let request = json!({
        "Image": "contract/create:v1",
        "Entrypoint": ["/bin/contract"],
        "Cmd": ["one", "two words"],
        "Env": ["MODE=matrix"],
        "WorkingDir": "/work",
        "User": "123:456",
        "Hostname": "matrix-host",
        "Labels": {"contract": "create"},
        "StopSignal": "SIGUSR1",
        "StopTimeout": 17,
        "ExposedPorts": {"8080/tcp": {}},
        "HostConfig": {
            "ExtraHosts": ["peer:192.0.2.4"],
            "Memory": 67108864,
            "PidsLimit": 23,
            "NanoCpus": 1500000000,
            "ReadonlyRootfs": true,
            "NetworkMode": "none",
            "RestartPolicy": {"Name": "on-failure", "MaximumRetryCount": 4}
        }
    });
    let response = create(socket, "supported", request).await?;
    require(
        response.status == 201,
        &format!("supported create request was HTTP {}: {}", response.status, response.body),
    )?;
    let id = response.body["Id"].as_str().ok_or("create response omitted Id")?;
    let inspect = exchange(socket, "GET", &format!("/v1.43/containers/{id}/json"), None).await?;
    require(inspect.status == 200, "supported container inspect was not HTTP 200")?;

    let expected = [
        ("Path", json!("/bin/contract")),
        ("Args", json!(["one", "two words"])),
        ("Name", json!("/supported")),
        ("Config.Labels", json!({"contract": "create"})),
        ("Config.StopSignal", json!("SIGUSR1")),
        ("Config.StopTimeout", json!(17)),
        ("Config.ExposedPorts", json!({"8080/tcp": {}})),
        ("HostConfig.ExtraHosts", json!(["peer:192.0.2.4"])),
        ("HostConfig.ReadonlyRootfs", json!(true)),
        ("HostConfig.NetworkMode", json!("none")),
        ("HostConfig.RestartPolicy", json!({"Name": "on-failure", "MaximumRetryCount": 4})),
    ];
    for (path, value) in expected {
        require(at(&inspect.body, path) == Some(&value), &format!("inspect changed {path}"))?;
    }
    remove_container(socket, id).await
}

async fn zero_compatibility_fields_are_inert(socket: &Path) -> Result<(), Error> {
    let inert = [
        ("TopNull", Value::Null),
        ("TopFalse", json!(false)),
        ("TopZero", json!(0)),
        ("TopString", json!("")),
        ("TopArray", json!([])),
        ("TopObject", json!({})),
    ];
    let host_inert = [
        ("Privileged", json!(false)),
        ("CpuShares", json!(0)),
        ("CapAdd", json!([])),
        ("SecurityOpt", json!([])),
        ("LogConfig", json!({})),
    ];
    let mut request = base_request();
    let object = request.as_object_mut().ok_or("base request was not an object")?;
    object.extend(inert.into_iter().map(|(name, value)| (name.into(), value)));
    object.insert(
        "HostConfig".into(),
        Value::Object(host_inert.into_iter().map(|(name, value)| (name.into(), value)).collect()),
    );
    let response = create(socket, "inert", request).await?;
    require(response.status == 201, "zero-valued compatibility fields were not inert")?;
    remove_container(socket, response.body["Id"].as_str().ok_or("create response omitted Id")?).await
}

async fn meaningful_unsupported_fields_are_refused(socket: &Path) -> Result<(), Error> {
    let cases = [
        Refusal::top("Domainname", json!("example.test")),
        Refusal::top("MacAddress", json!("02:42:ac:11:00:02")),
        Refusal::host("Privileged", json!(true)),
        Refusal::host("CapAdd", json!(["NET_ADMIN"])),
        Refusal::host("Links", json!(["database:database"])),
        Refusal::host("SecurityOpt", json!(["no-new-privileges"])),
        Refusal::host("ShmSize", json!(67108864)),
        Refusal::host("LogConfig", json!({"Type": "json-file"})),
    ];
    for case in cases {
        let before = container_ids(socket).await?;
        let response = create(socket, &format!("refuse-{}", case.field.to_lowercase()), case.request()).await?;
        require(response.status == 501, &format!("meaningful {} was not HTTP 501", case.field))?;
        require(
            response.body["message"].as_str().is_some_and(|message| message.contains(case.field)),
            &format!("{} refusal did not identify the field", case.field),
        )?;
        require(container_ids(socket).await? == before, &format!("{} refusal created a container", case.field))?;
    }
    Ok(())
}

async fn failed_create_releases_anonymous_volumes(socket: &Path) -> Result<(), Error> {
    let owner = create(socket, "rollback-owner", base_request()).await?;
    require(owner.status == 201, "rollback owner create was not HTTP 201")?;
    let before = volume_names(socket).await?;
    let mut request = base_request();
    request["HostConfig"] = json!({"Binds": ["/anonymous"]});
    let conflict = create(socket, "rollback-owner", request).await?;
    require(conflict.status == 409, "duplicate-name create was not HTTP 409")?;
    require(volume_names(socket).await? == before, "failed create leaked an anonymous volume")?;
    remove_container(socket, owner.body["Id"].as_str().ok_or("create response omitted Id")?).await
}

struct Refusal {
    field: &'static str,
    host: bool,
    value: Value,
}

impl Refusal {
    fn top(field: &'static str, value: Value) -> Self {
        Self { field, host: false, value }
    }

    fn host(field: &'static str, value: Value) -> Self {
        Self { field, host: true, value }
    }

    fn request(&self) -> Value {
        let mut request = base_request();
        if self.host {
            request["HostConfig"] = Value::Object(Map::from_iter([(self.field.into(), self.value.clone())]));
        } else {
            request[self.field] = self.value.clone();
        }
        request
    }
}

fn base_request() -> Value {
    json!({"Image": "contract/create:v1"})
}

async fn create(socket: &Path, name: &str, request: Value) -> Result<Response, Error> {
    exchange(
        socket,
        "POST",
        &format!("/v1.43/containers/create?name={name}"),
        Some(serde_json::to_vec(&request)?),
    )
    .await
}

async fn container_ids(socket: &Path) -> Result<BTreeSet<String>, Error> {
    let response = exchange(socket, "GET", "/v1.43/containers/json?all=true", None).await?;
    require(response.status == 200, "container list was not HTTP 200")?;
    Ok(response
        .body
        .as_array()
        .ok_or("container list was not an array")?
        .iter()
        .filter_map(|value| value["Id"].as_str().map(str::to_owned))
        .collect())
}

async fn volume_names(socket: &Path) -> Result<BTreeSet<String>, Error> {
    let response = exchange(socket, "GET", "/v1.43/volumes", None).await?;
    require(response.status == 200, "volume list was not HTTP 200")?;
    Ok(response.body["Volumes"]
        .as_array()
        .ok_or("volume list omitted Volumes")?
        .iter()
        .filter_map(|value| value["Name"].as_str().map(str::to_owned))
        .collect())
}

async fn remove_container(socket: &Path, id: &str) -> Result<(), Error> {
    let response = exchange(socket, "DELETE", &format!("/v1.43/containers/{id}?force=true&v=true"), None).await?;
    require(response.status == 204, "container cleanup was not HTTP 204")
}

fn at<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(value, |value, segment| value.get(segment))
}

struct Response {
    status: u16,
    body: Value,
}

async fn exchange(socket: &Path, method: &str, target: &str, body: Option<Vec<u8>>) -> Result<Response, Error> {
    let body = body.unwrap_or_default();
    let mut request = format!(
        "{method} {target} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    request.extend_from_slice(&body);
    let response = raw_http(socket, &request).await?;
    let (head, body) = response.split_once("\r\n\r\n").ok_or("HTTP response omitted its body")?;
    let status = head
        .split_whitespace()
        .nth(1)
        .ok_or("HTTP response omitted status")?
        .parse()?;
    let body = if body.is_empty() { Value::Null } else { serde_json::from_str(body)? };
    Ok(Response { status, body })
}
