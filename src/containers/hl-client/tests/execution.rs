use std::path::Path;

use hl_client::api::{Channel, Session, Size};
use hl_client::model::{Attachment, ExecAttach, ExecConfig, ExecStart};
use hl_client::{Client, Config, Error};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

async fn request(stream: &mut UnixStream) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        let count = stream.read(&mut chunk).await.unwrap();
        assert_ne!(count, 0, "request ended before its declared body");
        bytes.extend_from_slice(&chunk[..count]);
        let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&bytes[..end + 4]).to_ascii_lowercase();
        let length = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length: "))
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        if bytes.len() >= end + 4 + length {
            return bytes;
        }
    }
}

fn parts(request: &[u8]) -> (&str, &[u8]) {
    let end = request.windows(4).position(|window| window == b"\r\n\r\n").unwrap();
    (std::str::from_utf8(&request[..end + 4]).unwrap(), &request[end + 4..])
}

fn listener(socket: &Path) -> UnixListener {
    UnixListener::bind(socket).unwrap()
}

#[tokio::test]
async fn create_and_inspect_use_docker_paths_and_shared_wire_casing() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("daemon.sock");
    let listener = listener(&socket);
    let server = tokio::spawn(async move {
        let (mut create, _) = listener.accept().await.unwrap();
        let captured = request(&mut create).await;
        let (headers, body) = parts(&captured);
        assert!(headers.starts_with("POST /v1.43/containers/example%2Fname/exec HTTP/1.1\r\n"));
        assert_eq!(
            std::str::from_utf8(body).unwrap(),
            r#"{"AttachStdin":true,"AttachStdout":true,"AttachStderr":false,"DetachKeys":"ctrl-x","Tty":false,"Env":["A=B"],"Cmd":["echo","ok"],"Privileged":false,"User":"1000","WorkingDir":"/work"}"#
        );
        create
            .write_all(
                b"HTTP/1.1 201 Created\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: 16\r\n\r\n{\"Id\":\"exec-id\"}",
            )
            .await
            .unwrap();

        let (mut inspect, _) = listener.accept().await.unwrap();
        let captured = request(&mut inspect).await;
        let (headers, body) = parts(&captured);
        assert!(headers.starts_with("GET /v1.43/exec/exec%2Fid/json HTTP/1.1\r\n"));
        assert!(body.is_empty());
        let response = r#"{"ID":"exec-id","ContainerID":"container-id","Running":false,"ExitCode":7,"Pid":0,"CanRemove":true,"DetachKeys":"ctrl-x","OpenStdin":true,"OpenStdout":true,"OpenStderr":false,"ProcessConfig":{"arguments":["ok"],"entrypoint":"echo","privileged":false,"tty":false,"user":"1000"}}"#;
        inspect
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{response}",
                    response.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });

    let client = Client::unix(&socket).unwrap();
    let config = ExecConfig {
        attach: Attachment {
            stdin: true,
            stdout: true,
            stderr: false,
        },
        detach_keys: "ctrl-x".into(),
        env: Some(vec!["A=B".into()]),
        command: vec!["echo".into(), "ok".into()],
        user: "1000".into(),
        working_dir: "/work".into(),
        ..Default::default()
    };
    let created = client.executions().create("example/name", &config).await.unwrap();
    assert_eq!(created.id, "exec-id");
    let inspected = client.executions().inspect("exec/id").await.unwrap();
    assert_eq!(inspected.id, "exec-id");
    assert_eq!(inspected.exit_code, 7);
    assert_eq!(inspected.process.entrypoint, "echo");
    server.await.unwrap();
}

#[tokio::test]
async fn list_and_logs_use_finite_exec_endpoints() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("daemon.sock");
    let listener = listener(&socket);
    let server = tokio::spawn(async move {
        let (mut peer, _) = listener.accept().await.unwrap();
        assert!(parts(&request(&mut peer).await).0.starts_with("GET /v1.43/exec/json?limit=7 HTTP/1.1\r\n"));
        let response = r#"{"executions":[],"truncated":false}"#;
        peer.write_all(format!("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();

        let (mut peer, _) = listener.accept().await.unwrap();
        assert!(parts(&request(&mut peer).await).0.starts_with("GET /v1.43/exec/exec%2Fid/logs HTTP/1.1\r\n"));
        let response = r#"{"stdout":[0,255],"stderr":[3]}"#;
        peer.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
    });
    let client = Client::unix(&socket).unwrap();
    assert!(client.executions().list(7).await.unwrap().executions.is_empty());
    let logs = client.executions().logs("exec/id").await.unwrap();
    assert_eq!(logs.stdout, [0, 255]);
    assert_eq!(logs.stderr, [3]);
    server.await.unwrap();
}

#[tokio::test]
async fn wait_uses_the_blocking_exec_endpoint_and_returns_the_terminal_status() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("daemon.sock");
    let listener = listener(&socket);
    let server = tokio::spawn(async move {
        let (mut peer, _) = listener.accept().await.unwrap();
        let captured = request(&mut peer).await;
        let (headers, body) = parts(&captured);
        assert!(headers.starts_with("POST /v1.43/exec/exec%2Fid/wait HTTP/1.1\r\n"));
        assert!(body.is_empty());
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        peer.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 17\r\n\r\n{\"StatusCode\":23}",
        )
        .await
        .unwrap();
    });

    let status = Client::with_config(Config::unix(&socket).timeout(std::time::Duration::from_millis(10)))
        .unwrap()
        .executions()
        .wait("exec/id")
        .await
        .unwrap();

    assert_eq!(status.status_code, 23);
    server.await.unwrap();
}

#[tokio::test]
async fn attached_start_posts_json_then_decodes_frames_bidirectionally() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("daemon.sock");
    let listener = listener(&socket);
    let server = tokio::spawn(async move {
        let (mut peer, _) = listener.accept().await.unwrap();
        let request = request(&mut peer).await;
        let (headers, body) = parts(&request);
        assert!(headers.starts_with("POST /v1.43/exec/exec%2Fid/start HTTP/1.1\r\n"));
        let lower = headers.to_ascii_lowercase();
        assert!(lower.contains("connection: upgrade\r\n"));
        assert!(lower.contains("upgrade: tcp\r\n"));
        assert_eq!(body, br#"{"Detach":false,"Tty":false,"KillOnDisconnect":false}"#);
        peer.write_all(b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: tcp\r\n\r\n")
            .await
            .unwrap();
        let mut input = [0; 4];
        peer.read_exact(&mut input).await.unwrap();
        assert_eq!(&input, b"in\n\n");
        peer.write_all(&[1, 0, 0, 0, 0, 0, 0, 3, b'o', b'u', b't'])
            .await
            .unwrap();
    });

    let client = Client::unix(&socket).unwrap();
    let mut session = client
        .executions()
        .start("exec/id", &ExecStart::default())
        .await
        .unwrap();
    assert!(matches!(&session, Session::Pipes(_)));
    session.write(b"in\n\n").await.unwrap();
    let output = session.next().await.unwrap().unwrap();
    assert_eq!(output.channel(), Channel::Stdout);
    assert_eq!(output.bytes(), b"out".as_slice());
    server.await.unwrap();
}

#[tokio::test]
async fn terminal_attachment_splits_for_concurrent_input_and_output() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("daemon.sock");
    let listener = listener(&socket);
    let server = tokio::spawn(async move {
        let (mut peer, _) = listener.accept().await.unwrap();
        let captured = request(&mut peer).await;
        let (headers, body) = parts(&captured);
        assert!(headers.starts_with("POST /v1.43/exec/terminal/start HTTP/1.1\r\n"));
        assert_eq!(body, br#"{"Detach":false,"Tty":true,"KillOnDisconnect":false}"#);
        peer.write_all(b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: tcp\r\n\r\n")
            .await
            .unwrap();
        peer.write_all(b"ready\r\n").await.unwrap();
        let mut input = [0; 5];
        peer.read_exact(&mut input).await.unwrap();
        assert_eq!(&input, b"help\n");
    });

    let client = Client::unix(&socket).unwrap();
    let session = client
        .executions()
        .start(
            "terminal",
            &ExecStart {
                tty: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let (mut input, mut output) = session.into_terminal().unwrap();
    let (written, received) = tokio::join!(input.write(b"help\n"), output.next());
    written.unwrap();
    assert_eq!(received.unwrap().unwrap().bytes(), b"ready\r\n".as_slice());
    server.await.unwrap();
}

#[tokio::test]
async fn running_terminal_reattach_uses_the_distinct_attach_operation() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("daemon.sock");
    let listener = listener(&socket);
    let server = tokio::spawn(async move {
        let (mut peer, _) = listener.accept().await.unwrap();
        let captured = request(&mut peer).await;
        let (headers, body) = parts(&captured);
        assert!(headers.starts_with("POST /v1.43/exec/restored%2Fterminal/attach HTTP/1.1\r\n"));
        assert_eq!(body, br#"{"Tty":true,"KillOnDisconnect":true,"ConsoleSize":[31,97]}"#);
        peer.write_all(
            b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: tcp\r\n\r\nrestored-once\r\n",
        )
        .await
        .unwrap();
    });

    let client = Client::unix(&socket).unwrap();
    let session = client
        .executions()
        .attach(
            "restored/terminal",
            &ExecAttach {
                tty: true,
                kill_on_disconnect: true,
                console_size: Some([31, 97]),
            },
        )
        .await
        .unwrap();
    let (_input, mut output) = session.into_terminal().unwrap();
    assert_eq!(
        output.next().await.unwrap().unwrap().bytes(),
        b"restored-once\r\n".as_slice()
    );
    server.await.unwrap();
}

#[tokio::test]
async fn framed_pipe_attachment_rejects_terminal_split() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("daemon.sock");
    let listener = listener(&socket);
    let server = tokio::spawn(async move {
        let (mut peer, _) = listener.accept().await.unwrap();
        let _ = request(&mut peer).await;
        peer.write_all(b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: tcp\r\n\r\n")
            .await
            .unwrap();
    });

    let session = Client::unix(&socket)
        .unwrap()
        .executions()
        .start("pipes", &ExecStart::default())
        .await
        .unwrap();
    assert!(matches!(
        session.into_terminal(),
        Err(Error::Protocol(message)) if message.contains("framed pipe")
    ));
    server.await.unwrap();
}

#[tokio::test]
async fn detached_start_posts_json_without_requesting_an_upgrade() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("daemon.sock");
    let listener = listener(&socket);
    let server = tokio::spawn(async move {
        let (mut peer, _) = listener.accept().await.unwrap();
        let request = request(&mut peer).await;
        let (headers, body) = parts(&request);
        assert!(headers.starts_with("POST /v1.43/exec/exec-id/start HTTP/1.1\r\n"));
        assert!(!headers.to_ascii_lowercase().contains("upgrade: tcp"));
        assert_eq!(body, br#"{"Detach":true,"Tty":false,"KillOnDisconnect":false}"#);
        peer.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
    });

    Client::unix(&socket)
        .unwrap()
        .executions()
        .start_detached(
            "exec-id",
            &ExecStart {
                detach: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn start_modes_reject_mismatched_detach_policy_before_io() {
    let root = TempDir::new().unwrap();
    let client = Client::unix(root.path().join("absent.sock")).unwrap();
    let detached = ExecStart {
        detach: true,
        ..Default::default()
    };
    assert!(matches!(
        client.executions().start("exec", &detached).await,
        Err(Error::Protocol(message)) if message.contains("Detach=true")
    ));
    assert!(matches!(
        client
            .executions()
            .start_detached("exec", &ExecStart::default())
            .await,
        Err(Error::Protocol(message)) if message.contains("Detach=true")
    ));
}

#[tokio::test]
async fn resize_encodes_docker_height_before_width() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("daemon.sock");
    let listener = listener(&socket);
    let server = tokio::spawn(async move {
        let (mut peer, _) = listener.accept().await.unwrap();
        let captured = request(&mut peer).await;
        let (headers, body) = parts(&captured);
        assert!(headers.starts_with("POST /v1.43/exec/exec%2Fid/resize?h=41&w=109 HTTP/1.1\r\n"));
        assert!(body.is_empty());
        peer.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
    });

    Client::unix(&socket)
        .unwrap()
        .executions()
        .resize("exec/id", Size::new(41, 109).unwrap())
        .await
        .unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn signal_targets_only_the_encoded_execution() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("daemon.sock");
    let listener = listener(&socket);
    let server = tokio::spawn(async move {
        let (mut peer, _) = listener.accept().await.unwrap();
        let captured = request(&mut peer).await;
        let (headers, body) = parts(&captured);
        assert!(headers.starts_with("POST /v1.43/exec/exec%2Fid/kill?signal=HUP HTTP/1.1\r\n"));
        assert!(body.is_empty());
        peer.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
    });

    Client::unix(&socket)
        .unwrap()
        .executions()
        .signal("exec/id", "HUP")
        .await
        .unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn remove_uses_the_execution_resource() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("daemon.sock");
    let listener = listener(&socket);
    let server = tokio::spawn(async move {
        let (mut peer, _) = listener.accept().await.unwrap();
        let captured = request(&mut peer).await;
        let (headers, body) = parts(&captured);
        assert!(headers.starts_with("DELETE /v1.43/exec/exec%2Fid HTTP/1.1\r\n"));
        assert!(body.is_empty());
        peer.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
    });

    Client::unix(&socket)
        .unwrap()
        .executions()
        .remove("exec/id")
        .await
        .unwrap();
    server.await.unwrap();
}
