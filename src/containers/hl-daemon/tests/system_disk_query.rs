//! Docker API 1.43 disk-usage type projection over the raw HTTP boundary.

use hl_container::{Config, Containers};
use hl_daemon::Daemon;
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::UnixStream,
    sync::oneshot,
    time::{sleep, timeout},
};

const TIMEOUT: Duration = Duration::from_secs(15);

async fn wait_for_path(socket: &Path) -> Result<(), Box<dyn std::error::Error>> {
    timeout(TIMEOUT, async {
        while !socket.exists() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| "daemon socket startup timed out".into())
}

async fn request(socket: &Path, query: &str) -> Result<(String, Value), Box<dyn std::error::Error>> {
    request_at(socket, "/v1.43", query).await
}

async fn request_at(socket: &Path, version: &str, query: &str) -> Result<(String, Value), Box<dyn std::error::Error>> {
    timeout(TIMEOUT, async {
        let mut stream = UnixStream::connect(socket).await?;
        let request =
            format!("GET {version}/system/df{query} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await?;
        if response.len() > 1024 * 1024 {
            return Err("system disk response exceeded one MiB".into());
        }
        let response = String::from_utf8(response)?;
        let (head, body) = response
            .split_once("\r\n\r\n")
            .ok_or("system disk response omitted its body")?;
        let status = head
            .lines()
            .next()
            .ok_or("system disk response omitted status")?
            .to_owned();
        Ok((status, serde_json::from_str(body)?))
    })
    .await
    .map_err(|_| "system disk exchange timed out")?
}

fn projection(containers: Value, images: Value, volumes: Value, build_cache: Value) -> Value {
    json!({
        "LayersSize": 0,
        "Images": images,
        "Containers": containers,
        "Volumes": volumes,
        "BuildCache": build_cache,
    })
}

#[tokio::test]
async fn type_projection() -> Result<(), Box<dyn std::error::Error>> {
    let work = TempDir::new()?;
    let containers = Containers::builder(Config::new(work.path().join("state")))
        .build()
        .await?;
    let socket = work.path().join("daemon.sock");
    let (shutdown, stopped) = oneshot::channel();
    let server = tokio::spawn(Daemon::new(containers).server(&socket).serve_with_shutdown(async move {
        let _ = stopped.await;
    }));
    wait_for_path(&socket).await?;

    let result = async {
        let all = projection(json!([]), json!([]), json!([]), json!([]));
        let cases = [
            ("", all.clone()),
            ("?filters=%7B%7D", all),
            (
                "?type=container",
                projection(json!([]), Value::Null, Value::Null, Value::Null),
            ),
            (
                "?type=image",
                projection(Value::Null, json!([]), Value::Null, Value::Null),
            ),
            (
                "?type=volume",
                projection(Value::Null, Value::Null, json!([]), Value::Null),
            ),
            (
                "?type=build-cache",
                projection(Value::Null, Value::Null, Value::Null, json!([])),
            ),
            (
                "?type=build%2Dcache",
                projection(Value::Null, Value::Null, Value::Null, json!([])),
            ),
            (
                "?type=volume&type=container",
                projection(json!([]), Value::Null, json!([]), Value::Null),
            ),
            (
                "?type=volume&type=volume",
                projection(Value::Null, Value::Null, json!([]), Value::Null),
            ),
        ];
        for (query, expected) in cases {
            let (status, body) = request(&socket, query).await?;
            if !status.starts_with("HTTP/1.1 200") {
                return Err(format!("{query:?} returned {status}").into());
            }
            if body != expected {
                return Err(format!("{query:?} returned {body}, expected {expected}").into());
            }
        }

        for (query, message) in [
            ("?type=volume&type=network", "unknown object type: network"),
            ("?type=", "unknown object type: "),
            ("?type=image,volume", "unknown object type: image,volume"),
        ] {
            let (status, body) = request(&socket, query).await?;
            if !status.starts_with("HTTP/1.1 400") {
                return Err(format!("{query:?} returned {status}").into());
            }
            if body != json!({"message": message}) {
                return Err(format!("{query:?} returned unexpected error {body}").into());
            }
        }

        let all = projection(json!([]), json!([]), json!([]), json!([]));
        for version in ["/v1.24", "/v1.41"] {
            for query in ["?type=volume", "?type=", "?type=network"] {
                let (status, body) = request_at(&socket, version, query).await?;
                if !status.starts_with("HTTP/1.1 200") || body != all {
                    return Err(format!("legacy {version}{query} returned {status}: {body}").into());
                }
            }
        }

        let (status, body) = request_at(&socket, "", "?type=volume").await?;
        let volume = projection(Value::Null, Value::Null, json!([]), Value::Null);
        if !status.starts_with("HTTP/1.1 200") || body != volume {
            return Err(format!("unversioned projection returned {status}: {body}").into());
        }
        let (status, body) = request_at(&socket, "", "?type=network").await?;
        if !status.starts_with("HTTP/1.1 400") || body != json!({"message":"unknown object type: network"}) {
            return Err(format!("unversioned validation returned {status}: {body}").into());
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = shutdown.send(());
    server.await??;
    result
}
