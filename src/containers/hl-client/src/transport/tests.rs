use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use super::*;

async fn read_request(stream: &mut UnixStream) {
    let mut request = Vec::new();
    let mut byte = [0_u8; 1];
    while !request.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).await.expect("request byte");
        request.push(byte[0]);
    }
}

fn transport(root: &tempfile::TempDir) -> Transport {
    Transport::new(Config::unix(root.path().join("daemon.sock")))
}

#[tokio::test]
async fn buffered_requests_reuse_one_connection() {
    let root = tempfile::TempDir::new().unwrap();
    let listener = UnixListener::bind(root.path().join("daemon.sock")).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        for _ in 0..2 {
            read_request(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK")
                .await
                .unwrap();
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(50), listener.accept())
                .await
                .is_err()
        );
    });
    let client = transport(&root);
    assert_eq!(client.get_unversioned("/_ping").await.unwrap(), "OK");
    assert_eq!(client.get_unversioned("/_ping").await.unwrap(), "OK");
    server.await.unwrap();
}

#[tokio::test]
async fn reconnects_when_keep_alive_peer_closed() {
    let root = tempfile::TempDir::new().unwrap();
    let listener = UnixListener::bind(root.path().join("daemon.sock")).unwrap();
    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_request(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\nOK")
                .await
                .unwrap();
        }
    });
    let client = transport(&root);
    assert_eq!(client.get_unversioned("/_ping").await.unwrap(), "OK");
    tokio::task::yield_now().await;
    assert_eq!(client.get_unversioned("/_ping").await.unwrap(), "OK");
    server.await.unwrap();
}

#[tokio::test]
async fn timeout_cancels_and_discards_connection() {
    let root = tempfile::TempDir::new().unwrap();
    let listener = UnixListener::bind(root.path().join("daemon.sock")).unwrap();
    let server = tokio::spawn(async move {
        let (mut stalled, _) = listener.accept().await.unwrap();
        read_request(&mut stalled).await;
        let (mut healthy, _) = listener.accept().await.unwrap();
        read_request(&mut healthy).await;
        healthy
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK")
            .await
            .unwrap();
    });
    let config = Config::unix(root.path().join("daemon.sock")).timeout(Duration::from_millis(30));
    let client = Transport::new(config);
    assert!(matches!(
        client.get_unversioned("/_ping").await,
        Err(Error::Timeout)
    ));
    assert_eq!(client.get_unversioned("/_ping").await.unwrap(), "OK");
    server.await.unwrap();
}

#[tokio::test]
async fn stream_is_pull_based_and_bounds_each_frame() {
    let root = tempfile::TempDir::new().unwrap();
    let listener = UnixListener::bind(root.path().join("daemon.sock")).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_request(&mut stream).await;
        stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nab\r\n4\r\ncdef\r\n0\r\n\r\n",
                )
                .await
                .unwrap();
    });
    let client = Transport::new(Config::unix(root.path().join("daemon.sock")).response_limit(3));
    let mut response = client.stream(Method::GET, "/events").await.unwrap();
    assert_eq!(response.next_chunk().await.unwrap().unwrap(), "ab");
    assert!(matches!(
        response.next_chunk().await,
        Err(Error::ResponseTooLarge { limit: 3 })
    ));
    server.await.unwrap();
}

#[tokio::test]
async fn upgraded_connection_is_bidirectional() {
    let root = tempfile::TempDir::new().unwrap();
    let listener = UnixListener::bind(root.path().join("daemon.sock")).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_request(&mut stream).await;
        stream
                .write_all(
                    b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: tcp\r\n\r\nhello",
                )
                .await
                .unwrap();
        let mut reply = [0_u8; 5];
        stream.read_exact(&mut reply).await.unwrap();
        assert_eq!(&reply, b"world");
    });
    let client = transport(&root);
    let mut stream = client.upgrade(Method::POST, "/attach").await.unwrap();
    let mut greeting = [0_u8; 5];
    stream.read_exact(&mut greeting).await.unwrap();
    assert_eq!(&greeting, b"hello");
    stream.write_all(b"world").await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn archive_upload_forwards_chunks_before_reader_eof() {
    let root = tempfile::TempDir::new().unwrap();
    let listener = UnixListener::bind(root.path().join("daemon.sock")).unwrap();
    let (first_seen, first_arrived) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 128];
        let mut first_seen = Some(first_seen);
        loop {
            let count = stream.read(&mut chunk).await.unwrap();
            assert_ne!(count, 0, "upload ended before second chunk");
            bytes.extend_from_slice(&chunk[..count]);
            if first_seen.is_some() && bytes.windows(b"first".len()).any(|value| value == b"first")
            {
                first_seen.take().unwrap().send(()).unwrap();
            }
            if bytes
                .windows(b"second".len())
                .any(|value| value == b"second")
                && bytes.ends_with(b"0\r\n\r\n")
            {
                break;
            }
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\n{\"ok\":true}")
            .await
            .unwrap();
    });
    let client = std::sync::Arc::new(transport(&root));
    let (mut writer, reader) = tokio::io::duplex(32);
    let upload = {
        let client = client.clone();
        tokio::spawn(async move {
            client
                .upload::<_, serde_json::Value>("/images/load", reader)
                .await
                .unwrap()
        })
    };
    writer.write_all(b"first").await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), first_arrived)
        .await
        .expect("first chunk reached peer before reader EOF")
        .unwrap();
    writer.write_all(b"second").await.unwrap();
    writer.shutdown().await.unwrap();
    assert_eq!(upload.await.unwrap(), serde_json::json!({"ok": true}));
    server.await.unwrap();
}
