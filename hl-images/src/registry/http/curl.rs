//! The `curl`-subprocess machinery: a shared header-capturing runner (`run_curl`) plus the small
//! helpers it composes with (temp header path, status parsing, auth-header args) and the [`Resp`]
//! it returns. Everything the HTTP verbs need to talk to a registry lives here.

use crate::Error;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

pub(in crate::registry) struct Resp {
    pub(in crate::registry) status: u16,
    pub(in crate::registry) headers: String,
    pub(in crate::registry) body: Vec<u8>,
}

// curl args are strings. Shared by every curl path (`run_curl` and the blob `download_to_file`) so the
// two can't drift: `--connect-timeout` bounds only the TCP/TLS connect phase (fail fast on an
// unreachable/firewalled registry), while `--max-time` caps the whole transfer.
pub(super) const CONNECT_TIMEOUT_SECS: &str = "10";
pub(super) const MAX_TIME_SECS: &str = "600";

static SEQ: AtomicU64 = AtomicU64::new(0);
fn tmp_headers() -> std::path::PathBuf {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("hl-reg-{}-{n}.hdr", std::process::id()))
}

pub(super) fn run_curl(args: &[String]) -> Result<Resp, Error> {
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

pub(super) fn with_auth(mut args: Vec<String>, accept: Option<&str>, token: Option<&str>) -> Vec<String> {
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
}
