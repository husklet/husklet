//! The HTTP request verbs — the typed `get`/`head`/`post`/`put`/`download` surface the client calls.
//! Each composes a curl argument vector (via [`with_auth`]) and hands it to [`run_curl`], except the
//! blob `download_to_file`, which streams straight to disk so the byte count is observable.

use super::*;
use crate::Error;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// A process-unique temp path `<tmp>/hl-reg-body-<pid>-<seq>.bin` for a request body, so concurrent
/// `put_bytes` calls in ONE process never share (and clobber) the same file.
fn reg_body_tmp() -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("hl-reg-body-{}-{n}.bin", std::process::id()))
}

pub(in crate::registry) fn get(
    url: &str,
    accept: Option<&str>,
    token: Option<&str>,
) -> Result<Resp, Error> {
    // `-L` FOLLOW REDIRECTS: registries (Docker Hub, ECR, GCR, …) serve blob GETs — including the
    // image CONFIG blob — as a 307 to pre-signed CDN storage. Without following it we'd get the 307
    // (not 200) and `config_blob` would fail, silently dropping the image's Entrypoint/Cmd/Env/User/
    // WorkingDir (this is exactly the layer path's `download_to_file`, which already uses `-sSL`).
    // Safe on manifest/non-redirected GETs (curl only redirects on a 3xx), and curl strips the
    // Authorization header on a cross-host redirect (the CDN URL is pre-signed, so no auth is needed).
    Curl::execute(&with_auth(vec!["-L".into(), url.into()], accept, token))
}
pub(in crate::registry) struct Request;
impl Request {
    pub(in crate::registry) fn get_with_basic(
        url: &str,
        creds: Option<&Credentials>,
    ) -> Result<Resp, Error> {
        let mut args = vec![url.to_string()];
        if let Some(c) = creds {
            args.push("-u".into());
            args.push(format!("{}:{}", c.username, c.password));
        }
        Curl::execute(&args)
    }
    pub(in crate::registry) fn head(url: &str, token: Option<&str>) -> Result<u16, Error> {
        Curl::execute(&with_auth(vec!["-I".into(), url.into()], None, token)).map(|r| r.status)
    }
    pub(in crate::registry) fn post(url: &str, token: Option<&str>) -> Result<Resp, Error> {
        Curl::execute(&with_auth(
            vec!["-X".into(), "POST".into(), url.into()],
            None,
            token,
        ))
    }
}
pub(in crate::registry) fn put_file(
    url: &str,
    file: &Path,
    content_type: &str,
    token: Option<&str>,
) -> Result<Resp, Error> {
    // `-T` (upload-file) STREAMS the body from disk and sets Content-Length from the file size —
    // unlike `--data-binary @file`, which buffers the entire file in memory (OOMs on multi-GB layers).
    let args = with_auth(
        vec![
            "-X".into(),
            "PUT".into(),
            "-H".into(),
            format!("Content-Type: {content_type}"),
            "-T".into(),
            file.display().to_string(),
            url.into(),
        ],
        None,
        token,
    );
    Curl::execute(&args)
}
pub(in crate::registry) fn put_bytes(
    url: &str,
    body: &[u8],
    content_type: &str,
    token: Option<&str>,
) -> Result<Resp, Error> {
    let tmp = reg_body_tmp();
    std::fs::write(&tmp, body).map_err(|e| Error::Registry(e.to_string()))?;
    let r = put_file(url, &tmp, content_type, token);
    let _ = std::fs::remove_file(&tmp);
    r
}
/// Download a blob to `dest`, calling `progress` with the bytes-so-far while curl runs so the caller
/// can stream a live download bar. curl writes straight to disk (`-o`); we poll the file size every
/// ~150ms until the process exits, then report the final size. Landing the blob on disk (vs piping)
/// is what makes the byte count observable.
pub(in crate::registry) fn download_to_file(
    url: &str,
    token: Option<&str>,
    dest: &Path,
    progress: &mut dyn FnMut(u64),
) -> Result<(), Error> {
    let mut cmd = Command::new("curl");
    // `-f` FAIL ON HTTP ERROR: without it, curl writes a 404/500 error body straight to `dest` and exits
    // 0, so an HTTP error page would be saved as if it were the layer blob (then fed to tar). With `-f`
    // curl emits nothing on >=400 and exits non-zero, so the download surfaces as an Err.
    cmd.arg("-f")
        .arg("-sSL")
        .arg("--connect-timeout")
        .arg(CONNECT_TIMEOUT_SECS)
        .arg("--max-time")
        .arg(MAX_TIME_SECS);
    if let Some(t) = token {
        cmd.arg("-H").arg(format!("Authorization: Bearer {t}"));
    }
    cmd.arg("-o").arg(dest).arg(url);
    let mut child = cmd
        .spawn()
        .map_err(|e| Error::Registry(format!("curl: {e}")))?;
    let file_len = |p: &Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    loop {
        match child
            .try_wait()
            .map_err(|e| Error::Registry(e.to_string()))?
        {
            Some(st) => {
                if !st.success() {
                    // `-f` failed the transfer (e.g. HTTP >=400): make sure no partial/empty file is left
                    // behind to be mistaken for a valid blob, and surface the failure.
                    let _ = std::fs::remove_file(dest);
                    return Err(Error::Registry(format!("curl blob download failed ({st})")));
                }
                progress(file_len(dest)); // final, exact size
                return Ok(());
            }
            None => {
                progress(file_len(dest));
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    // Finding 7: two `put_bytes`-class calls must get DISTINCT temp body files (was a single fixed
    // `hl-reg-body-<pid>.bin`, so concurrent calls clobbered each other's body).
    #[test]
    fn reg_body_tmp_is_unique_per_call() {
        let a = reg_body_tmp();
        let b = reg_body_tmp();
        assert_ne!(a, b, "each request body gets its own temp path");
        assert!(a.starts_with(std::env::temp_dir()));
        // writing different bytes to each path does not clobber the other.
        std::fs::write(&a, b"AAA").unwrap();
        std::fs::write(&b, b"BBBB").unwrap();
        assert_eq!(std::fs::read(&a).unwrap(), b"AAA");
        assert_eq!(std::fs::read(&b).unwrap(), b"BBBB");
        let _ = std::fs::remove_file(&a);
        let _ = std::fs::remove_file(&b);
    }

    // Finding 6: a layer download that gets a 404 must return Err (not save the error body as a blob).
    #[test]
    fn download_to_file_404_is_error_not_a_blob() {
        // one-shot local server that answers every request with 404 + an error body.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf);
                let body = b"404 page not found";
                let resp = format!(
                    "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes());
                let _ = sock.write_all(body);
            }
        });

        let dest = reg_body_tmp(); // borrow the unique-temp helper for a scratch path
        let url = format!("http://127.0.0.1:{port}/v2/x/blobs/sha256:deadbeef");
        let r = download_to_file(&url, None, &dest, &mut |_| {});
        let _ = handle.join();

        assert!(r.is_err(), "a 404 layer download must be an Err");
        assert!(
            !dest.exists(),
            "the HTTP error body must not be left on disk as a saved blob"
        );
        let _ = std::fs::remove_file(&dest);
    }
}
