//! The HTTP request verbs — the typed `get`/`head`/`post`/`put`/`download` surface the client calls.
//! Each composes a curl argument vector (via [`with_auth`]) and hands it to [`run_curl`], except the
//! blob `download_to_file`, which streams straight to disk so the byte count is observable.

use super::*;
use crate::Error;
use std::path::Path;
use std::process::Command;

pub(in crate::registry) fn get(url: &str, accept: Option<&str>, token: Option<&str>) -> Result<Resp, Error> {
    // `-L` FOLLOW REDIRECTS: registries (Docker Hub, ECR, GCR, …) serve blob GETs — including the
    // image CONFIG blob — as a 307 to pre-signed CDN storage. Without following it we'd get the 307
    // (not 200) and `config_blob` would fail, silently dropping the image's Entrypoint/Cmd/Env/User/
    // WorkingDir (this is exactly the layer path's `download_to_file`, which already uses `-sSL`).
    // Safe on manifest/non-redirected GETs (curl only redirects on a 3xx), and curl strips the
    // Authorization header on a cross-host redirect (the CDN URL is pre-signed, so no auth is needed).
    run_curl(&with_auth(vec!["-L".into(), url.into()], accept, token))
}
pub(in crate::registry) fn get_with_basic(url: &str, creds: Option<&Credentials>) -> Result<Resp, Error> {
    let mut args = vec![url.to_string()];
    if let Some(c) = creds {
        args.push("-u".into());
        args.push(format!("{}:{}", c.username, c.password));
    }
    run_curl(&args)
}
pub(in crate::registry) fn head(url: &str, token: Option<&str>) -> Result<u16, Error> {
    run_curl(&with_auth(vec!["-I".into(), url.into()], None, token)).map(|r| r.status)
}
pub(in crate::registry) fn post(url: &str, token: Option<&str>) -> Result<Resp, Error> {
    run_curl(&with_auth(
        vec!["-X".into(), "POST".into(), url.into()],
        None,
        token,
    ))
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
    run_curl(&args)
}
pub(in crate::registry) fn put_bytes(
    url: &str,
    body: &[u8],
    content_type: &str,
    token: Option<&str>,
) -> Result<Resp, Error> {
    let tmp = std::env::temp_dir().join(format!("dd-reg-body-{}.bin", std::process::id()));
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
    cmd.arg("-sSL")
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
        match child.try_wait().map_err(|e| Error::Registry(e.to_string()))? {
            Some(st) => {
                if !st.success() {
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
