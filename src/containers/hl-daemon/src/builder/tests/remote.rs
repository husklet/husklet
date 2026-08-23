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

/// A host with no CA store must still be able to build.
///
/// `Builder::build` calls [`RemoteSources::fetch`] on every build, and building an HTTP client
/// loads the host's certificate store. Nothing above states a CA requirement, so a distroless or
/// scratch image without `ca-certificates`, or a Nix build sandbox -- which sets
/// `SSL_CERT_FILE=/no-cert-file.crt` -- could build no image at all, and could not `ADD` from a
/// plain `http://` URL that involves no certificate from anybody.
///
/// The environment is what breaks, so the environment is what is varied, and it is varied against
/// **the test binary**: a `cargo test` wrapper sits between the caller and the process being
/// configured, and a sibling lane got `ok` from the wrapper spelling once and never reproduced it
/// while the binary spelling failed 4 times out of 4.
const ABSENT_CA_STORE_CHILD: &str = "HL_BUILDER_ABSENT_CA_STORE_CHILD";
/// Nix's own spelling for "this build has no certificates", and a path that cannot exist.
const NO_CA_STORE: &str = "/no-cert-file.crt";

/// Run one `#[ignore]`d body in a child process under a deliberately impoverished environment.
///
/// `name` is the **full** test path. A bare function name matches nothing, and a filter that
/// matches nothing exits 0 reporting `0 passed`, so the count is what is read here and not the
/// status -- this harness caught itself doing exactly that once already.
fn in_a_child(name: &str, environment: &[(&str, &str)]) {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([name, "--exact", "--ignored", "--nocapture", "--test-threads=1"])
        .env(ABSENT_CA_STORE_CHILD, "1")
        .env("SSL_CERT_FILE", NO_CA_STORE)
        // `SSL_CERT_DIR` is the other half of the pair the certificate loader consults; leaving the
        // host's value in place would let it find a store these tests claim is absent.
        .env("SSL_CERT_DIR", "/no-cert-directory");
    for (key, value) in environment {
        command.env(key, value);
    }
    let child = command.output().unwrap();
    let out = String::from_utf8_lossy(&child.stdout);
    let err = String::from_utf8_lossy(&child.stderr);
    assert!(
        out.contains("1 passed"),
        "{name} did not run exactly one test\nstatus: {}\nstdout:\n{out}\nstderr:\n{err}",
        child.status
    );
    assert!(
        child.status.success(),
        "{name} did not survive\nstatus: {}\nstdout:\n{out}\nstderr:\n{err}",
        child.status
    );
}

#[test]
fn a_build_needs_no_ca_store_until_a_source_needs_tls() {
    in_a_child("builder::tests::remote::remote_sources_on_a_host_with_no_ca_store", &[]);
}

/// A recipe with no remote source must construct nothing, and the only way to see that is to make
/// construction fail.
///
/// The CA store cannot show it any more: a recipe with no remote source selects the empty-root
/// client, which builds happily without one, so an eagerly-constructed client would be just as
/// green. `TMPDIR` can. A build that downloads nothing needs no download area, and a container with
/// a read-only rootfs and no writable temporary directory is a real deployment shape -- so pointing
/// `TMPDIR` at a path that does not exist both proves the laziness and asserts something true.
#[test]
fn a_recipe_with_no_remote_source_creates_no_download_area() {
    in_a_child(
        "builder::tests::remote::no_remote_source_needs_no_temporary_directory",
        &[("TMPDIR", "/no-such-temporary-directory")],
    );
}

#[tokio::test]
#[ignore = "re-executed by a_recipe_with_no_remote_source_creates_no_download_area with no TMPDIR"]
async fn no_remote_source_needs_no_temporary_directory() {
    assert_eq!(
        std::env::var("TMPDIR").ok().as_deref(),
        Some("/no-such-temporary-directory"),
        "this body only means anything under the parent's environment"
    );
    assert!(
        !std::env::temp_dir().exists(),
        "the temporary directory this body requires to be missing exists: {:?}",
        std::env::temp_dir()
    );

    let ordinary = Recipe::parse("FROM scratch\nCOPY payload.txt /payload.txt\n").unwrap();
    let none = RemoteSources::fetch(&ordinary).await.unwrap();
    assert_eq!(none.entries().count(), 0);
}

#[tokio::test]
#[ignore = "re-executed by a_build_needs_no_ca_store_until_a_source_needs_tls with no CA store"]
async fn remote_sources_on_a_host_with_no_ca_store() {
    assert_eq!(
        std::env::var("SSL_CERT_FILE").ok().as_deref(),
        Some(NO_CA_STORE),
        "this body only means anything under the parent's environment"
    );
    assert!(
        std::env::var_os(ABSENT_CA_STORE_CHILD).is_some(),
        "this body only means anything under the parent's environment"
    );

    // A plain-`http://` source needs no certificate from anybody, and now needs no CA store either.
    let payload = b"no-ca-store".to_vec();
    let url = serve_once(payload.clone(), "payload").await;
    let checksum = hl_images::Digest::sha256(&payload).encoded().to_owned();
    let plain = Recipe::parse(&format!(
        "FROM scratch\nADD --checksum=sha256:{checksum} {url} /payload\n"
    ))
    .unwrap();
    let fetched = RemoteSources::fetch(&plain).await.unwrap();
    let file = fetched.get(&url).unwrap();
    assert_eq!(std::fs::read(file.root().join(file.name())).unwrap(), payload);

    // A TLS source genuinely needs the store, and says so. Nothing is sent: the client fails to be
    // built before any name is resolved, which is why `registry.invalid` never has to resolve.
    let tls = Recipe::parse("FROM scratch\nADD https://registry.invalid/payload /payload\n").unwrap();
    let refused = RemoteSources::fetch(&tls).await.unwrap_err().to_string();
    assert!(
        refused.contains("No CA certificates were loaded from the system"),
        "a TLS ADD with no CA store did not name the CA store: {refused}"
    );
    assert!(
        refused.contains("ca-certificates") && refused.contains("SSL_CERT_FILE"),
        "a TLS ADD with no CA store did not say how to fix it: {refused}"
    );
}
