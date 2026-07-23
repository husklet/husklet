use std::path::Path;

use hl_client::model::{CreateContainer, List};
use hl_client::{Client, Config, Error};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;

fn peer(socket: &Path, response: &'static str) -> tokio::task::JoinHandle<String> {
    let listener = UnixListener::bind(socket).expect("bind test socket");
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept request");
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let count = stream.read(&mut chunk).await.expect("read request");
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..count]);
            if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end + 4]);
                let length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if bytes.len() >= end + 4 + length {
                    break;
                }
            }
        }
        stream
            .write_all(response.as_bytes())
            .await
            .expect("write response");
        String::from_utf8(bytes).expect("ASCII request")
    })
}

#[tokio::test]
async fn ping_uses_unversioned_endpoint() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("daemon.sock");
    let request = peer(&socket, "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK");
    Client::unix(&socket).unwrap().ping().await.unwrap();
    assert!(request
        .await
        .unwrap()
        .starts_with("GET /_ping HTTP/1.1\r\n"));
}

#[tokio::test]
async fn create_versions_and_encodes_name_and_body() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("daemon.sock");
    let response = "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: 26\r\n\r\n{\"Id\":\"abc\",\"Warnings\":[]}";
    let captured = peer(&socket, response);
    let create = CreateContainer {
        image: "alpine:latest".into(),
        cmd: Some(vec!["echo".into(), "hi".into()]),
        ..Default::default()
    };
    let result = Client::unix(&socket)
        .unwrap()
        .containers()
        .create(&create, Some("hello world"))
        .await
        .unwrap();
    assert_eq!(result.id, "abc");
    let request = captured.await.unwrap();
    assert!(request.starts_with("POST /v1.43/containers/create?name=hello%20world HTTP/1.1\r\n"));
    assert!(request.contains("\"Image\":\"alpine:latest\""));
}

#[tokio::test]
async fn docker_error_preserves_status_and_message() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("daemon.sock");
    let response = "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: 21\r\n\r\n{\"message\":\"missing\"}";
    let captured = peer(&socket, response);
    let error = Client::unix(&socket)
        .unwrap()
        .containers()
        .inspect("gone")
        .await
        .unwrap_err();
    assert!(
        matches!(error, Error::Docker { status: http::StatusCode::NOT_FOUND, ref message } if message == "missing")
    );
    captured.await.unwrap();
}

#[tokio::test]
async fn response_limit_applies_before_allocating_body() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("daemon.sock");
    let captured = peer(&socket, "HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n");
    let client = Client::with_config(Config::unix(&socket).response_limit(8)).unwrap();
    assert!(matches!(
        client.ping().await.unwrap_err(),
        Error::ResponseTooLarge { limit: 8 }
    ));
    captured.await.unwrap();
}

#[tokio::test]
async fn list_encodes_typed_docker_filters() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("daemon.sock");
    let captured = peer(
        &socket,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n[]",
    );
    let selection = List::default().all().name("worker").status("running");
    let containers = Client::unix(&socket)
        .unwrap()
        .containers()
        .list(selection)
        .await
        .unwrap();
    assert!(containers.is_empty());
    assert!(captured.await.unwrap().starts_with(
        "GET /v1.43/containers/json?all=true&filters=%7B%22name%22%3A%5B%22worker%22%5D%2C%22status%22%3A%5B%22running%22%5D%7D HTTP/1.1\r\n"
    ));
}
