use std::collections::BTreeMap;
use std::path::Path;

use hl_client::model::{VolumeCreate, VolumePrune};
use hl_client::{Client, Error};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;

fn peer(socket: &Path, status: &str, body: &str) -> tokio::task::JoinHandle<String> {
    let listener = UnixListener::bind(socket).expect("bind test socket");
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept request");
        let mut bytes = Vec::new();
        let mut chunk = [0; 1024];
        loop {
            let count = stream.read(&mut chunk).await.expect("read request");
            assert_ne!(count, 0, "request ended before its declared body");
            bytes.extend_from_slice(&chunk[..count]);
            let Some(end) = bytes.windows(4).position(|value| value == b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&bytes[..end + 4]);
            let length = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0);
            if bytes.len() >= end + 4 + length {
                break;
            }
        }
        stream.write_all(response.as_bytes()).await.unwrap();
        String::from_utf8(bytes).expect("ASCII request")
    })
}

fn body(request: &str) -> serde_json::Value {
    let (_, bytes) = request.split_once("\r\n\r\n").unwrap();
    serde_json::from_str(bytes).unwrap()
}

fn volume(name: &str) -> String {
    format!(
        r#"{{"CreatedAt":"2026-07-15T00:00:00.000000000Z","Driver":"local","Labels":{{"purpose":"test"}},"Mountpoint":"/state/volumes/{name}/_data","Name":"{name}","Options":{{}},"Scope":"local"}}"#
    )
}

#[tokio::test]
async fn create_uses_the_shared_request_and_decodes_the_volume() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("daemon.sock");
    let response = volume("cache");
    let captured = peer(&socket, "201 Created", &response);
    let request = VolumeCreate {
        name: "cache".into(),
        driver: "local".into(),
        driver_opts: BTreeMap::new(),
        labels: BTreeMap::from([("purpose".into(), "test".into())]),
        cluster_volume_spec: None,
        unsupported: BTreeMap::new(),
    };

    let created = Client::unix(&socket)
        .unwrap()
        .volumes()
        .create(&request)
        .await
        .unwrap();
    assert_eq!(created.name, "cache");
    assert_eq!(created.driver, "local");
    assert_eq!(created.labels["purpose"], "test");

    let captured = captured.await.unwrap();
    assert!(captured.starts_with("POST /v1.43/volumes/create HTTP/1.1\r\n"));
    assert_eq!(body(&captured), serde_json::to_value(request).unwrap());
}

#[tokio::test]
async fn list_and_inspect_preserve_docker_models_and_encode_names() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("daemon.sock");
    let listed = format!(r#"{{"Volumes":[{}],"Warnings":["degraded"]}}"#, volume("a"));
    let captured = peer(&socket, "200 OK", &listed);
    let client = Client::unix(&socket).unwrap();
    let result = client.volumes().list().await.unwrap();
    assert_eq!(result.volumes.len(), 1);
    assert_eq!(result.volumes[0].name, "a");
    assert_eq!(result.warnings, ["degraded"]);
    assert!(captured
        .await
        .unwrap()
        .starts_with("GET /v1.43/volumes HTTP/1.1\r\n"));

    let socket = root.path().join("inspect.sock");
    let inspected = volume("cache/name");
    let captured = peer(&socket, "200 OK", &inspected);
    let result = Client::unix(&socket)
        .unwrap()
        .volumes()
        .inspect("cache/name")
        .await
        .unwrap();
    assert_eq!(result.name, "cache/name");
    assert!(captured
        .await
        .unwrap()
        .starts_with("GET /v1.43/volumes/cache%2Fname HTTP/1.1\r\n"));
}

#[tokio::test]
async fn remove_encodes_force_and_preserves_conflict_status() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("daemon.sock");
    let captured = peer(&socket, "204 No Content", "");
    Client::unix(&socket)
        .unwrap()
        .volumes()
        .remove("cache/name", true)
        .await
        .unwrap();
    assert!(captured
        .await
        .unwrap()
        .starts_with("DELETE /v1.43/volumes/cache%2Fname?force=true HTTP/1.1\r\n"));

    let socket = root.path().join("conflict.sock");
    let captured = peer(&socket, "409 Conflict", r#"{"message":"volume is in use"}"#);
    let error = Client::unix(&socket)
        .unwrap()
        .volumes()
        .remove("cache", false)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::Docker {
            status: http::StatusCode::CONFLICT,
            ref message
        } if message == "volume is in use"
    ));
    captured.await.unwrap();
}

#[tokio::test]
async fn prune_posts_without_a_body_and_decodes_reclaimed_space() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("daemon.sock");
    let response = r#"{"VolumesDeleted":["one","two"],"SpaceReclaimed":42}"#;
    let captured = peer(&socket, "200 OK", response);
    let result = Client::unix(&socket)
        .unwrap()
        .volumes()
        .prune()
        .await
        .unwrap();
    assert_eq!(
        result,
        VolumePrune {
            volumes_deleted: vec!["one".into(), "two".into()],
            space_reclaimed: 42,
        }
    );
    let captured = captured.await.unwrap();
    assert!(captured.starts_with("POST /v1.43/volumes/prune HTTP/1.1\r\n"));
    assert_eq!(captured.split_once("\r\n\r\n").unwrap().1, "");
}
