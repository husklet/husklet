//! A daemon on a host with no CA store: it must serve, and it must diagnose.
//!
//! `hl_daemon::Daemon::new` builds an OCI registry source, and building an HTTP client loads the
//! host's CA store. On a host that has none, `reqwest::Client::new()` does not fail -- it panics:
//!
//! ```text
//! Client::new(): reqwest::Error { kind: Builder,
//!   source: General("No CA certificates were loaded from the system") }
//! ```
//!
//! So a daemon that was never going to fetch an image could not start at all. That is a deployment
//! defect, not a latency one: it is fatal in a Nix build sandbox, which sets
//! `SSL_CERT_FILE=/no-cert-file.crt`, and fatal in a distroless or scratch container image, which
//! ships no `ca-certificates`. `flake.nix`'s `alpineCompatibility` check papers over it today by
//! exporting a CA bundle into a sandbox that has no network.
//!
//! The environment is the thing that broke, so the environment is what is tested. A child process
//! is re-executed with `SSL_CERT_FILE` pointing at a path that does not exist, which is what the
//! Nix sandbox does and is indistinguishable, to the certificate loader, from a host with no store
//! at all. In that child the daemon must reach the point of answering on its socket, an image pull
//! must come back as an error naming the CA store rather than a panic or a silence, and the daemon
//! must still be serving afterwards.
//!
//! The pull needs no network and cannot reach one: the client fails to be *built*, before any name
//! is resolved or any byte is sent. That is also what makes the child self-validating -- on a host
//! where a CA store were somehow still reachable the same request would fail with a DNS or connect
//! error instead, and the diagnostic assertion would redden rather than quietly passing.

use hl_container::{Config, Containers};
use hl_daemon::Daemon;
use std::path::Path;
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tokio::time::{sleep, timeout};

type Error = Box<dyn std::error::Error>;

/// Set by the parent so the re-executed binary runs the child body rather than spawning again.
const CHILD: &str = "HL_DAEMON_ABSENT_CA_STORE_CHILD";
const CHILD_TEST: &str = "a_daemon_with_no_ca_store_serves_and_diagnoses";
/// Nix's own spelling for "this build has no certificates", and a path that cannot exist.
const NO_CA_STORE: &str = "/no-cert-file.crt";
const TIMEOUT: Duration = Duration::from_secs(30);

/// The certificate loader's own words. If a client is ever built successfully here, this is absent
/// and every assertion below that depends on it fails.
const CA_STORE: &str = "No CA certificates were loaded from the system";

#[test]
fn a_daemon_starts_and_pulls_report_without_a_system_ca_store() -> Result<(), Error> {
    let child = std::process::Command::new(std::env::current_exe()?)
        .args([CHILD_TEST, "--exact", "--ignored", "--nocapture", "--test-threads=1"])
        .env(CHILD, "1")
        .env("SSL_CERT_FILE", NO_CA_STORE)
        // `SSL_CERT_DIR` is the other half of the pair the certificate loader consults; leaving the
        // host's value in place would let it find a store the test is claiming is absent.
        .env("SSL_CERT_DIR", "/no-cert-directory")
        .output()?;
    let out = String::from_utf8_lossy(&child.stdout);
    let err = String::from_utf8_lossy(&child.stderr);

    // A filter that matches nothing exits 0 with `0 passed`, so the count is read, not the status.
    assert!(
        out.contains("1 passed"),
        "child did not run exactly one test\nstatus: {}\nstdout:\n{out}\nstderr:\n{err}",
        child.status
    );
    assert!(
        child.status.success(),
        "daemon startup with no CA store did not survive\nstatus: {}\nstdout:\n{out}\nstderr:\n{err}",
        child.status
    );
    Ok(())
}

#[tokio::test]
#[ignore = "re-executed by a_daemon_starts_and_pulls_report_without_a_system_ca_store with no CA store"]
async fn a_daemon_with_no_ca_store_serves_and_diagnoses() -> Result<(), Error> {
    assert_eq!(
        std::env::var("SSL_CERT_FILE").ok().as_deref(),
        Some(NO_CA_STORE),
        "this body only means anything under the parent's environment"
    );
    assert!(
        std::env::var_os(CHILD).is_some(),
        "this body only means anything under the parent's environment"
    );

    let work = TempDir::new()?;
    let containers = Containers::builder(Config::new(work.path().join("state")))
        .build()
        .await?;
    let socket = work.path().join("daemon.sock");
    let (shutdown, stopped) = oneshot::channel();

    // Construction and startup. This is the line that used to abort the process at frame 2507 of
    // `reqwest`, before the socket existed and before any log the operator could read.
    let server = tokio::spawn(Daemon::new(containers).server(&socket).serve_with_shutdown(async move {
        let _ = stopped.await;
    }));
    wait_for_socket(&socket).await?;
    assert!(
        http(
            &socket,
            b"GET /_ping HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        )
        .await?
        .starts_with("HTTP/1.1 200"),
        "a daemon with no CA store did not answer its socket"
    );

    // An operation that genuinely needs the network. `registry.invalid` is reserved by RFC 6761 and
    // never resolves, and nothing here tries to: the client fails to be built first.
    let pull = http(
        &socket,
        b"POST /v1.43/images/create?fromImage=registry.invalid/absent&tag=latest HTTP/1.1\r\n\
          Host: localhost\r\nConnection: close\r\n\r\n",
    )
    .await?;
    assert!(
        pull.contains(CA_STORE),
        "a pull with no CA store did not name the CA store: {pull}"
    );
    assert!(
        pull.contains("ca-certificates") && pull.contains("SSL_CERT_FILE"),
        "a pull with no CA store did not say how to fix it: {pull}"
    );

    // Reported, not died: an error the operator can read is only useful from a daemon still serving.
    assert!(
        http(
            &socket,
            b"GET /_ping HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        )
        .await?
        .starts_with("HTTP/1.1 200"),
        "the daemon did not survive reporting its missing CA store"
    );

    let _ = shutdown.send(());
    timeout(TIMEOUT, server)
        .await?
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    Ok(())
}

async fn wait_for_socket(socket: &Path) -> Result<(), Error> {
    timeout(TIMEOUT, async {
        while !socket.exists() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| "daemon socket never appeared".into())
}

async fn http(socket: &Path, request: &[u8]) -> Result<String, Error> {
    timeout(TIMEOUT, async {
        let mut stream = UnixStream::connect(socket).await?;
        stream.write_all(request).await?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await?;
        Ok::<_, Error>(String::from_utf8_lossy(&response).into_owned())
    })
    .await
    .map_err(|_| "daemon did not answer")?
}
