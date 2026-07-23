use std::path::Path;

use hl_client::api::{Channel, Session, Size};
use hl_client::{Client, Config, Error};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

async fn request(stream: &mut UnixStream) -> String {
    let mut bytes = Vec::new();
    let mut byte = [0; 1];
    while !bytes.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).await.unwrap();
        bytes.push(byte[0]);
    }
    String::from_utf8(bytes).unwrap()
}

fn listener(socket: &Path) -> UnixListener {
    UnixListener::bind(socket).unwrap()
}

#[tokio::test]
async fn session_upgrades_writes_closes_and_decodes_ordered_output() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("daemon.sock");
    let listener = listener(&socket);
    let server = tokio::spawn(async move {
        let (mut peer, _) = listener.accept().await.unwrap();
        let request = request(&mut peer).await;
        let lower = request.to_ascii_lowercase();
        assert!(request.starts_with(
            "POST /v1.43/containers/example%2Fname/attach?stream=true&stdin=true&stdout=true&stderr=true HTTP/1.1\r\n"
        ));
        assert!(lower.contains("connection: upgrade\r\n"));
        assert!(lower.contains("upgrade: tcp\r\n"));
        peer.write_all(
            b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: tcp\r\n\r\n",
        )
        .await
        .unwrap();

        let mut input = [0; 6];
        peer.read_exact(&mut input).await.unwrap();
        assert_eq!(&input, b"hello\n");
        assert_eq!(peer.read(&mut input).await.unwrap(), 0);

        peer.write_all(&[1, 0, 0]).await.unwrap();
        peer.write_all(&[0, 0, 0, 0, 3, b'o']).await.unwrap();
        peer.write_all(b"ut").await.unwrap();
        peer.write_all(&[2, 0, 0, 0, 0, 0, 0, 3]).await.unwrap();
        peer.write_all(b"err").await.unwrap();
    });

    let client = Client::unix(&socket).unwrap();
    let mut session = client
        .containers()
        .attach("example/name", true, true, true)
        .await
        .unwrap();
    assert!(matches!(&session, Session::Pipes(_)));
    session.write(b"hello\n").await.unwrap();
    session.close().await.unwrap();

    let stdout = session.next().await.unwrap().unwrap();
    assert_eq!(stdout.channel(), Channel::Stdout);
    assert_eq!(stdout.bytes(), b"out".as_slice());
    let stderr = session.next().await.unwrap().unwrap();
    assert_eq!(stderr.channel(), Channel::Stderr);
    assert_eq!(stderr.into_bytes(), b"err".as_slice());
    assert!(session.next().await.unwrap().is_none());
    server.await.unwrap();
}

#[tokio::test]
async fn terminal_session_preserves_raw_chunks_writes_and_eof() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("daemon.sock");
    let listener = listener(&socket);
    let server = tokio::spawn(async move {
        let (mut peer, _) = listener.accept().await.unwrap();
        let request = request(&mut peer).await;
        assert!(request.starts_with(
            "POST /v1.43/containers/terminal/attach?stream=true&stdin=true&stdout=true&stderr=true HTTP/1.1\r\n"
        ));
        peer.write_all(
            b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: tcp\r\n\r\n",
        )
        .await
        .unwrap();

        let mut input = [0; 6];
        peer.read_exact(&mut input).await.unwrap();
        assert_eq!(&input, b"hello\n");
        assert_eq!(peer.read(&mut input).await.unwrap(), 0);

        peer.write_all(b"merged ").await.unwrap();
        tokio::task::yield_now().await;
        peer.write_all(b"terminal\r\n").await.unwrap();
    });

    let client = Client::unix(&socket).unwrap();
    let mut session = client
        .containers()
        .attach_terminal("terminal", true)
        .await
        .unwrap();
    assert!(matches!(&session, Session::Terminal(_)));
    session.write(b"hello\n").await.unwrap();
    session.close().await.unwrap();

    let mut output = Vec::new();
    while let Some(chunk) = session.next().await.unwrap() {
        assert_eq!(chunk.channel(), Channel::Terminal);
        output.extend_from_slice(chunk.bytes());
    }
    assert_eq!(output, b"merged terminal\r\n");
    assert!(session.next().await.unwrap().is_none());
    server.await.unwrap();
}

#[tokio::test]
async fn session_rejects_a_frame_before_allocating_over_the_limit() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("daemon.sock");
    let listener = listener(&socket);
    let server = tokio::spawn(async move {
        let (mut peer, _) = listener.accept().await.unwrap();
        request(&mut peer).await;
        peer.write_all(
            b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: tcp\r\n\r\n\x01\0\0\0\0\0\0\x05",
        )
        .await
        .unwrap();
    });

    let client = Client::with_config(Config::unix(&socket).response_limit(4)).unwrap();
    let mut session = client
        .containers()
        .attach("example", false, true, true)
        .await
        .unwrap();
    assert!(matches!(
        session.next().await,
        Err(Error::ResponseTooLarge { limit: 4 })
    ));
    server.await.unwrap();
}

#[tokio::test]
async fn session_reports_truncated_frame_part() {
    for (frame, part) in [
        (&b"\x01\0\0"[..], "header"),
        (&b"\x01\0\0\0\0\0\0\x03x"[..], "payload"),
    ] {
        let root = TempDir::new().unwrap();
        let socket = root.path().join("daemon.sock");
        let listener = listener(&socket);
        let server = tokio::spawn(async move {
            let (mut peer, _) = listener.accept().await.unwrap();
            request(&mut peer).await;
            peer.write_all(
                b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: tcp\r\n\r\n",
            )
            .await
            .unwrap();
            peer.write_all(frame).await.unwrap();
        });

        let client = Client::unix(&socket).unwrap();
        let mut session = client
            .containers()
            .attach("example", false, true, true)
            .await
            .unwrap();
        assert!(matches!(
            session.next().await,
            Err(Error::Protocol(message)) if message == format!("truncated stream frame {part}")
        ));
        server.await.unwrap();
    }
}

#[tokio::test]
async fn container_resize_encodes_docker_height_before_width() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("daemon.sock");
    let listener = listener(&socket);
    let server = tokio::spawn(async move {
        let (mut peer, _) = listener.accept().await.unwrap();
        let request = request(&mut peer).await;
        assert!(request
            .starts_with("POST /v1.43/containers/example%2Fname/resize?h=33&w=120 HTTP/1.1\r\n"));
        peer.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
    });

    Client::unix(&socket)
        .unwrap()
        .containers()
        .resize("example/name", Size::new(33, 120).unwrap())
        .await
        .unwrap();
    server.await.unwrap();
}
