//! Thin `curl` wrappers plus small subprocess / header / base64 tools. Headers are captured to a temp
//! file (`-D`); the body goes to stdout (or a tar). The shelling-out is confined here — everything above
//! is ordinary typed code.

use super::*;
use crate::Error;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

pub(super) struct Resp {
    pub(super) status: u16,
    pub(super) headers: String,
    pub(super) body: Vec<u8>,
}

// curl args are strings. Shared by every curl path (`run_curl` and the blob `download_to_file`) so the
// two can't drift: `--connect-timeout` bounds only the TCP/TLS connect phase (fail fast on an
// unreachable/firewalled registry), while `--max-time` caps the whole transfer.
const CONNECT_TIMEOUT_SECS: &str = "10";
const MAX_TIME_SECS: &str = "600";

static SEQ: AtomicU64 = AtomicU64::new(0);
fn tmp_headers() -> std::path::PathBuf {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("dd-reg-{}-{n}.hdr", std::process::id()))
}

fn run_curl(args: &[String]) -> Result<Resp, Error> {
    let hdr = tmp_headers();
    let mut c = Command::new("curl");
    // `--connect-timeout` bounds only the TCP/TLS connect phase (not the transfer, which keeps the
    // 10-min `--max-time`), so an unreachable/firewalled registry fails fast instead of hanging. This
    // matters for the best-effort config refresh on a re-pull of an already-present tag:
    // when the dev host's egress is blocked, the refresh must give up quickly and keep the cached
    // config rather than stalling `docker pull` for minutes.
    c.arg("-sS")
        .arg("--connect-timeout")
        .arg(CONNECT_TIMEOUT_SECS)
        .arg("--max-time")
        .arg(MAX_TIME_SECS)
        .arg("-D")
        .arg(&hdr);
    for a in args {
        c.arg(a);
    }
    let out = c
        .output()
        .map_err(|e| Error::Registry(format!("curl: {e}")))?;
    let headers = std::fs::read_to_string(&hdr).unwrap_or_default();
    let _ = std::fs::remove_file(&hdr);
    if !out.status.success() && headers.is_empty() {
        return Err(Error::Registry(format!(
            "curl failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(Resp {
        status: status_of(&headers),
        headers,
        body: out.stdout,
    })
}
/// The status of the *last* response (after any redirects curl followed).
fn status_of(headers: &str) -> u16 {
    headers
        .lines()
        .rev()
        .find_map(|l| {
            l.strip_prefix("HTTP/")
                .and_then(|r| r.split_whitespace().nth(1))
        })
        .and_then(|c| c.parse().ok())
        .unwrap_or(0)
}

fn with_auth(mut args: Vec<String>, accept: Option<&str>, token: Option<&str>) -> Vec<String> {
    if let Some(a) = accept {
        args.push("-H".into());
        args.push(format!("Accept: {a}"));
    }
    if let Some(t) = token {
        args.push("-H".into());
        args.push(format!("Authorization: Bearer {t}"));
    }
    args
}

pub(super) fn get(url: &str, accept: Option<&str>, token: Option<&str>) -> Result<Resp, Error> {
    // `-L` FOLLOW REDIRECTS: registries (Docker Hub, ECR, GCR, …) serve blob GETs — including the
    // image CONFIG blob — as a 307 to pre-signed CDN storage. Without following it we'd get the 307
    // (not 200) and `config_blob` would fail, silently dropping the image's Entrypoint/Cmd/Env/User/
    // WorkingDir (this is exactly the layer path's `download_to_file`, which already uses `-sSL`).
    // Safe on manifest/non-redirected GETs (curl only redirects on a 3xx), and curl strips the
    // Authorization header on a cross-host redirect (the CDN URL is pre-signed, so no auth is needed).
    run_curl(&with_auth(vec!["-L".into(), url.into()], accept, token))
}
pub(super) fn get_with_basic(url: &str, creds: Option<&Credentials>) -> Result<Resp, Error> {
    let mut args = vec![url.to_string()];
    if let Some(c) = creds {
        args.push("-u".into());
        args.push(format!("{}:{}", c.username, c.password));
    }
    run_curl(&args)
}
pub(super) fn head(url: &str, token: Option<&str>) -> Result<u16, Error> {
    run_curl(&with_auth(vec!["-I".into(), url.into()], None, token)).map(|r| r.status)
}
pub(super) fn post(url: &str, token: Option<&str>) -> Result<Resp, Error> {
    run_curl(&with_auth(
        vec!["-X".into(), "POST".into(), url.into()],
        None,
        token,
    ))
}
pub(super) fn put_file(
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
pub(super) fn put_bytes(
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
pub(super) fn download_to_file(
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
/// Unpack a gzipped-tar layer blob from `src` into `rootfs` (`tar xzf`), unprivileged, on macOS.
///
/// dd flattens every OCI layer into one shared `rootfs` with sequential `tar` runs (no overlayfs), so
/// two macOS-specific hazards break `docker pull` for images that pull fine everywhere else:
///
///  1. Device nodes. Base layers (mysql:8.4, amazonlinux:2023, oraclelinux, …) ship char/fifo specials
///     under `dev/` (dev/console, dev/null, dev/ptmx, …). Unprivileged `mknod` fails "Operation not
///     permitted" and tar exits non-zero. We `--exclude 'dev/*'` so tar never tries — containers get a
///     fresh /dev synthesized by the engine at runtime, so the static nodes are never used (this is
///     what Docker's own userspace unpackers do).
///  2. Read-only directories a *previous* layer left behind. e.g. mysql's oraclelinux base ships
///     `etc/pki/ca-trust/extracted/pem/directory-hash/` as `dr-xr-xr-x` full of symlinks; libarchive
///     defers dir-mode restore so the layer that *creates* it extracts fine, but a later layer that
///     overwrites a symlink inside it (or a re-pull) can't `unlink` in the now-write-less dir →
///     "Can't unlink already-existing object: Permission denied" → the whole layer aborted. We recover
///     by re-adding owner-write to every dir in the rootfs and extracting the layer again (libarchive
///     re-restores the archive's own dir modes for dirs this layer contains; a dir it doesn't touch
///     just keeps owner-write, harmless for a rootfs whose processes run as root).
///
/// Real corruption (truncated/damaged gzip, "Unexpected EOF", "not in gzip format", "No space left")
/// is never swallowed — those still fail the pull.
pub(super) fn extract_targz(src: &Path, rootfs: &Path) -> Result<(), Error> {
    let attempt = || {
        Command::new("tar")
            .args(["--exclude", "dev/*", "--exclude", "./dev/*", "-xzf"])
            .arg(src)
            .arg("-C")
            .arg(rootfs)
            .output()
            .map_err(|e| Error::Archive(format!("tar: {e}")))
    };
    // Split tar's stderr into (needs a writable-dir retry?, fatal lines). Benign = unprivileged
    // mknod/ownership refusal or tar's trailing summary; retryable = a "Permission denied" overwrite
    // into a read-only dir; everything else is fatal.
    fn classify(stderr: &str) -> (bool, Vec<String>) {
        let (mut retry, mut fatal) = (false, Vec::new());
        for line in stderr.lines() {
            let l = line.trim();
            if l.is_empty()
                || l.contains("Operation not permitted")
                || l.contains("Cannot mknod")
                || l.contains("Error exit delayed from previous errors")
            {
                continue;
            }
            if l.contains("Permission denied") {
                retry = true;
                continue;
            }
            fatal.push(l.to_string());
        }
        (retry, fatal)
    }
    let out = attempt()?;
    if out.status.success() {
        return Ok(());
    }
    let (retry, fatal) = classify(&String::from_utf8_lossy(&out.stderr));
    if !fatal.is_empty() {
        return Err(Error::Archive(format!(
            "tar extract failed: {}",
            fatal.join("; ")
        )));
    }
    if !retry {
        return Ok(());
    } // only device-node noise — the layer's real content extracted fine
      // A read-only dir from an earlier layer is blocking this layer's overwrites: make every dir in the
      // rootfs owner-writable and extract the layer again.
    let _ = Command::new("find")
        .arg(rootfs)
        .args(["-type", "d", "-exec", "chmod", "u+w", "{}", "+"])
        .output();
    let out2 = attempt()?;
    if out2.status.success() {
        return Ok(());
    }
    let (_, fatal2) = classify(&String::from_utf8_lossy(&out2.stderr));
    if fatal2.is_empty() {
        Ok(())
    } else {
        Err(Error::Archive(format!(
            "tar extract failed after making dirs writable: {}",
            fatal2.join("; ")
        )))
    }
}

// ---- small subprocess / header / base64 tools shared across the module ----

pub(super) fn run(prog: &str, args: &[&str]) -> Result<String, Error> {
    let out = Command::new(prog)
        .args(args)
        .output()
        .map_err(|e| Error::Other(format!("{prog}: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let detail = if stderr.trim().is_empty() {
            format!("exited with {}", out.status)
        } else {
            stderr.trim().to_string()
        };
        return Err(Error::Other(format!("{prog} {args:?} failed: {detail}")));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub(super) fn header(headers: &str, name: &str) -> Option<String> {
    let want = format!("{}:", name.to_ascii_lowercase());
    headers
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with(&want))
        .and_then(|l| l.split_once(':'))
        .map(|(_, v)| v.trim().to_string())
}
/// Resolve a possibly-relative `Location` against the registry base origin.
pub(super) fn absolute(location: &str, base_v2: &str) -> String {
    if location.starts_with("http") {
        return location.to_string();
    }
    let origin = base_v2.split("/v2/").next().unwrap_or(base_v2);
    format!("{origin}{location}")
}

pub(super) fn base64_decode(s: &str) -> Option<Vec<u8>> {
    // docker uses standard or URL-safe base64; do it without a crate
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut table = [255u8; 256];
    for (i, &c) in A.iter().enumerate() {
        table[c as usize] = i as u8;
    }
    table[b'-' as usize] = 62;
    table[b'_' as usize] = 63;
    let mut bits = 0u32;
    let mut nbits = 0;
    let mut out = Vec::new();
    for &c in s.as_bytes() {
        if c == b'=' || c == b'\n' || c == b'\r' {
            continue;
        }
        let v = table[c as usize];
        if v == 255 {
            return None;
        }
        bits = (bits << 6) | v as u32;
        nbits += 6;
        if nbits >= 8 {
            nbits -= 8;
            out.push((bits >> nbits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- status_of: parse the HTTP status out of a raw -D header blob ----

    #[test]
    fn status_of_normal_response() {
        assert_eq!(status_of("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\n"), 200);
        assert_eq!(status_of("HTTP/1.1 301 Moved Permanently\r\nLocation: /x\r\n\r\n"), 301);
        assert_eq!(status_of("HTTP/1.1 404 Not Found\r\n\r\n"), 404);
        assert_eq!(status_of("HTTP/1.1 401 Unauthorized\r\n\r\n"), 401);
        // HTTP/2 has no "OK" reason phrase after the code; nth(1) still lands on the code.
        assert_eq!(status_of("HTTP/2 200\r\n\r\n"), 200);
    }

    #[test]
    fn status_of_redirect_chain_returns_last() {
        // curl -D appends each response's headers; status_of scans in REVERSE (.rev()) and returns
        // the LAST HTTP status line — the final response after redirects, not the 301/307.
        let chain = "HTTP/1.1 301 Moved Permanently\r\nLocation: https://cdn/x\r\n\r\n\
                     HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\n\r\n";
        assert_eq!(status_of(chain), 200);

        // A chain whose final hop is an error resolves to that error, not the intermediate 307.
        let to_404 = "HTTP/1.1 307 Temporary Redirect\r\nLocation: /gone\r\n\r\n\
                      HTTP/1.1 404 Not Found\r\n\r\n";
        assert_eq!(status_of(to_404), 404);
    }

    #[test]
    fn status_of_empty_or_garbage_is_zero() {
        // No "HTTP/" line -> find_map None -> unwrap_or(0). This is the sentinel the callers treat as
        // "no usable response".
        assert_eq!(status_of(""), 0);
        assert_eq!(status_of("not headers at all\r\ngarbage\r\n"), 0);
        // A truncated status line with no code also falls through to 0.
        assert_eq!(status_of("HTTP/1.1\r\n"), 0);
    }

    // ---- with_auth: append -H Accept / -H Authorization curl args ----

    #[test]
    fn with_auth_none_leaves_args_unchanged() {
        let base = vec!["-L".to_string(), "https://reg/v2/x".to_string()];
        assert_eq!(with_auth(base.clone(), None, None), base);
    }

    #[test]
    fn with_auth_accept_only() {
        let out = with_auth(vec!["url".to_string()], Some("application/vnd.oci.image.manifest.v1+json"), None);
        assert_eq!(
            out,
            vec![
                "url".to_string(),
                "-H".to_string(),
                "Accept: application/vnd.oci.image.manifest.v1+json".to_string(),
            ]
        );
    }

    #[test]
    fn with_auth_token_only() {
        let out = with_auth(vec!["url".to_string()], None, Some("tok"));
        assert_eq!(
            out,
            vec![
                "url".to_string(),
                "-H".to_string(),
                "Authorization: Bearer tok".to_string(),
            ]
        );
    }

    #[test]
    fn with_auth_both_accept_then_authorization() {
        // Accept is appended before Authorization (impl order); each as a separate -H / value pair.
        let out = with_auth(vec!["url".to_string()], Some("application/json"), Some("tok"));
        assert_eq!(
            out,
            vec![
                "url".to_string(),
                "-H".to_string(),
                "Accept: application/json".to_string(),
                "-H".to_string(),
                "Authorization: Bearer tok".to_string(),
            ]
        );
    }

    // ---- tmp_headers: unique temp path under the system temp dir ----

    #[test]
    fn tmp_headers_is_unique_and_in_temp_dir() {
        let a = tmp_headers();
        let b = tmp_headers();
        assert!(a.starts_with(std::env::temp_dir()), "path under temp_dir");
        assert_ne!(a, b, "atomic SEQ suffix makes each call distinct");
        assert!(a.extension().and_then(|e| e.to_str()) == Some("hdr"));
    }

    // ---- header: case-insensitive header lookup, trimmed value ----

    #[test]
    fn header_lookup_is_case_insensitive_and_trimmed() {
        let h = "Content-Type: text/plain\r\nDocker-Content-Digest: sha256:abc123\r\n";
        // name match ignores case; value is trimmed of the leading space.
        assert_eq!(header(h, "content-type"), Some("text/plain".to_string()));
        assert_eq!(header(h, "Content-Type"), Some("text/plain".to_string()));
        // a value containing a colon is preserved (split_once splits on the FIRST colon only).
        assert_eq!(
            header(h, "docker-content-digest"),
            Some("sha256:abc123".to_string())
        );
        // absent header -> None
        assert_eq!(header(h, "location"), None);
    }

    // ---- absolute: resolve a Location against the registry origin ----

    #[test]
    fn absolute_passes_through_absolute_urls() {
        assert_eq!(
            absolute("https://cdn.example.com/blob", "https://reg.example.com/v2/lib/ubuntu"),
            "https://cdn.example.com/blob"
        );
    }

    #[test]
    fn absolute_prepends_origin_for_relative() {
        // origin = everything before "/v2/"
        assert_eq!(
            absolute("/v2/lib/ubuntu/blobs/x", "https://reg.example.com/v2/lib/ubuntu"),
            "https://reg.example.com/v2/lib/ubuntu/blobs/x"
        );
        // base without "/v2/" -> the whole base is treated as the origin.
        assert_eq!(
            absolute("/path", "https://reg.example.com"),
            "https://reg.example.com/path"
        );
    }

    // ---- base64_decode: standard + URL-safe alphabets, no crate ----

    #[test]
    fn base64_decode_standard_with_padding() {
        // standard base64 of "foo:bar" (a docker registry basic-auth token shape)
        assert_eq!(base64_decode("Zm9vOmJhcg=="), Some(b"foo:bar".to_vec()));
    }

    #[test]
    fn base64_decode_no_padding_and_url_safe_alphabet() {
        // no '=' padding still decodes
        assert_eq!(base64_decode("aGVsbG8"), Some(b"hello".to_vec()));
        // URL-safe '-'/'_' map to 62/63: "-_8" -> [0xFB, 0xFF]
        assert_eq!(base64_decode("-_8"), Some(vec![0xFBu8, 0xFF]));
    }

    #[test]
    fn base64_decode_rejects_invalid_chars() {
        // a char outside both alphabets -> None
        assert_eq!(base64_decode("@@@"), None);
        // whitespace (\r/\n) inside the blob is skipped, not rejected
        assert_eq!(base64_decode("Zm9v\r\nOmJhcg=="), Some(b"foo:bar".to_vec()));
    }
}
