use std::path::Path;

use hl_client::Client;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;

fn peer(socket: &Path) -> tokio::task::JoinHandle<String> {
    let listener = UnixListener::bind(socket).unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = vec![0; 4096];
        let count = stream.read(&mut bytes).await.unwrap();
        stream
            .write_all(b"HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: 19\r\n\r\n{\"Id\":\"sha256:new\"}")
            .await
            .unwrap();
        String::from_utf8(bytes[..count].to_vec()).unwrap()
    })
}

#[tokio::test]
async fn commit_uses_typed_response_and_encoded_query() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("daemon.sock");
    let captured = peer(&socket);
    let result = Client::unix(&socket)
        .unwrap()
        .images()
        .commit("container/name", "example/repo", Some("v1"), true)
        .await
        .unwrap();
    assert_eq!(result.id, "sha256:new");
    let request = captured.await.unwrap();
    assert!(request.starts_with(
        "POST /v1.43/commit?container=container%2Fname&repo=example%2Frepo&tag=v1&pause=true HTTP/1.1"
    ));
}
