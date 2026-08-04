use super::super::copy::Copy;
use super::super::remote::RemoteSources;
use hl_images::build::Recipe;
use hl_images::snapshot::{Ownership, Ownerships};

async fn serve_once(response: Vec<u8>, name: &str) -> String {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 1024];
        let _ = socket.read(&mut request).await.unwrap();
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        socket.write_all(&response).await.unwrap();
    });
    format!("http://{address}/{name}")
}

#[tokio::test]
async fn remote_add_fetches_validates_and_keeps_archives_opaque() {
    use std::os::unix::fs::PermissionsExt as _;

    let mut archive_bytes = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut archive_bytes);
        let mut header = tar::Header::new_gnu();
        header.set_size(6);
        header.set_mode(0o644);
        header.set_cksum();
        archive.append_data(&mut header, "inside", &b"inside"[..]).unwrap();
        archive.finish().unwrap();
    }
    let url = serve_once(archive_bytes.clone(), "archive.tar").await;
    let checksum = hl_images::Digest::sha256(&archive_bytes).encoded().to_owned();
    let recipe = Recipe::parse(&format!(
        "FROM scratch\nADD --checksum=sha256:{checksum} {url} /artifact.tar\n"
    ))
    .unwrap();
    let remotes = RemoteSources::fetch(&recipe).await.unwrap();
    let digest = *remotes.entries().find(|(source, _)| *source == url).unwrap().1;
    let remote = remotes.get(&url).unwrap();
    assert_eq!(hl_images::Digest::from(digest).encoded(), checksum);
    assert_eq!(std::fs::read(remote.root().join(remote.name())).unwrap(), archive_bytes);

    let selected = [remote.name().to_owned()];
    let destination = tempfile::tempdir().unwrap();
    let mut ownerships = Ownerships::memory();
    Copy {
        source: remote.root(),
        sources: &selected,
        target: "/artifact.tar",
        directory: "/",
        destination: destination.path(),
        unpack: false,
        mode: Some(0o600),
        owner: Some(Ownership { uid: 12, gid: 34 }),
        excludes: &[],
        parents: false,
    }
    .apply(&mut ownerships)
    .unwrap();
    assert_eq!(
        std::fs::read(destination.path().join("artifact.tar")).unwrap(),
        archive_bytes
    );
    assert!(!destination.path().join("inside").exists());
    assert_eq!(
        std::fs::metadata(destination.path().join("artifact.tar"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(ownerships.get("artifact.tar"), Some(Ownership { uid: 12, gid: 34 }));

    let url = serve_once(archive_bytes, "file").await;
    let bad = Recipe::parse(&format!(
        "FROM scratch\nADD --checksum=sha256:{} {url} /file\n",
        "0".repeat(64)
    ))
    .unwrap();
    assert!(
        RemoteSources::fetch(&bad)
            .await
            .unwrap_err()
            .to_string()
            .contains("checksum mismatch")
    );
}
