use std::collections::BTreeMap;
use std::path::Path;

use hl_client::model::{
    ConfigFrom, EndpointConfig, EndpointIpam, Ipam, IpamConfig, NetworkConnect, NetworkCreate,
    NetworkDisconnect, NetworkPrune,
};
use hl_client::Client;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;

fn peer(socket: &Path, status: &str, body: &str) -> tokio::task::JoinHandle<String> {
    let listener = UnixListener::bind(socket).unwrap();
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let mut chunk = [0; 1024];
        loop {
            let count = stream.read(&mut chunk).await.unwrap();
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
        String::from_utf8(bytes).unwrap()
    })
}

fn body(request: &str) -> serde_json::Value {
    serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap()
}

fn network(name: &str) -> String {
    format!(
        r#"{{"Name":"{name}","Id":"0123456789abcdef0123456789abcdef","Created":"2026-07-15T00:00:00.000000000Z","Scope":"local","Driver":"none","EnableIPv6":false,"IPAM":{{"Driver":"default","Config":[]}},"Internal":true,"Attachable":false,"Ingress":false,"ConfigFrom":{{"Network":""}},"ConfigOnly":false,"Containers":{{}},"Options":{{}},"Labels":{{"purpose":"test"}}}}"#
    )
}

#[tokio::test]
async fn create_uses_shared_models_and_exact_docker_casing() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("daemon.sock");
    let captured = peer(
        &socket,
        "201 Created",
        r#"{"Id":"0123456789abcdef0123456789abcdef","Warning":""}"#,
    );
    let request = NetworkCreate {
        name: "isolated".into(),
        check_duplicate: true,
        driver: "none".into(),
        internal: true,
        labels: BTreeMap::from([("purpose".into(), "test".into())]),
        ipam: Ipam::default(),
        config_from: Some(ConfigFrom::default()),
        ..Default::default()
    };
    let result = Client::unix(&socket)
        .unwrap()
        .networks()
        .create(&request)
        .await
        .unwrap();
    assert_eq!(result.id, "0123456789abcdef0123456789abcdef");
    let captured = captured.await.unwrap();
    assert!(captured.starts_with("POST /v1.43/networks/create HTTP/1.1\r\n"));
    assert_eq!(body(&captured), serde_json::to_value(request).unwrap());
}

#[tokio::test]
async fn list_and_inspect_decode_models_and_encode_references() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("list.sock");
    let captured = peer(&socket, "200 OK", &format!("[{}]", network("isolated")));
    let listed = Client::unix(&socket)
        .unwrap()
        .networks()
        .list()
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "isolated");
    assert!(captured
        .await
        .unwrap()
        .starts_with("GET /v1.43/networks HTTP/1.1\r\n"));

    let socket = root.path().join("inspect.sock");
    let captured = peer(&socket, "200 OK", &network("team/net"));
    let inspected = Client::unix(&socket)
        .unwrap()
        .networks()
        .inspect("team/net")
        .await
        .unwrap();
    assert_eq!(inspected.name, "team/net");
    assert!(captured
        .await
        .unwrap()
        .starts_with("GET /v1.43/networks/team%2Fnet HTTP/1.1\r\n"));
}

#[tokio::test]
async fn connect_and_disconnect_post_shared_requests_to_encoded_paths() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("connect.sock");
    let captured = peer(&socket, "200 OK", "");
    let connect = NetworkConnect {
        container: "container/name".into(),
        endpoint_config: Some(EndpointConfig {
            ipam: Some(EndpointIpam {
                ipv4_address: "10.0.0.9".into(),
                ..Default::default()
            }),
            aliases: vec!["database".into()],
            ..Default::default()
        }),
        ..Default::default()
    };
    Client::unix(&socket)
        .unwrap()
        .networks()
        .connect("network/name", &connect)
        .await
        .unwrap();
    let captured = captured.await.unwrap();
    assert!(captured.starts_with("POST /v1.43/networks/network%2Fname/connect HTTP/1.1\r\n"));
    assert_eq!(body(&captured), serde_json::to_value(connect).unwrap());

    let socket = root.path().join("disconnect.sock");
    let captured = peer(&socket, "200 OK", "");
    let disconnect = NetworkDisconnect {
        container: "container/name".into(),
        force: true,
        ..Default::default()
    };
    Client::unix(&socket)
        .unwrap()
        .networks()
        .disconnect("network/name", &disconnect)
        .await
        .unwrap();
    let captured = captured.await.unwrap();
    assert!(captured.starts_with("POST /v1.43/networks/network%2Fname/disconnect HTTP/1.1\r\n"));
    assert_eq!(body(&captured), serde_json::to_value(disconnect).unwrap());
}

#[tokio::test]
async fn remove_and_prune_use_exact_paths_and_decode_results() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("remove.sock");
    let captured = peer(&socket, "204 No Content", "");
    Client::unix(&socket)
        .unwrap()
        .networks()
        .remove("network/name", true)
        .await
        .unwrap();
    assert!(captured
        .await
        .unwrap()
        .starts_with("DELETE /v1.43/networks/network%2Fname?force=true HTTP/1.1\r\n"));

    let socket = root.path().join("prune.sock");
    let captured = peer(&socket, "200 OK", r#"{"NetworksDeleted":["one","two"]}"#);
    let result = Client::unix(&socket)
        .unwrap()
        .networks()
        .prune()
        .await
        .unwrap();
    assert_eq!(
        result,
        NetworkPrune {
            networks_deleted: vec!["one".into(), "two".into()]
        }
    );
    let captured = captured.await.unwrap();
    assert!(captured.starts_with("POST /v1.43/networks/prune HTTP/1.1\r\n"));
    assert_eq!(captured.split_once("\r\n\r\n").unwrap().1, "");
}

#[test]
fn shared_ipam_types_remain_directly_composable() {
    let ipam = Ipam {
        driver: "default".into(),
        options: None,
        config: vec![IpamConfig {
            subnet: "10.0.0.0/24".into(),
            gateway: "10.0.0.1".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    assert_eq!(ipam.config[0].subnet, "10.0.0.0/24");
}
