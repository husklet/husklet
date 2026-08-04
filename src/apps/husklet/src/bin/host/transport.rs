use std::io::{Read, Write};

const MAX_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

pub struct LocalHttp<'a> {
    socket: &'a std::path::Path,
}

impl<'a> LocalHttp<'a> {
    pub fn new(socket: &'a std::path::Path) -> Self {
        Self { socket }
    }

    pub fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> std::io::Result<T> {
        if !path.starts_with('/')
            || path
                .bytes()
                .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "daemon request target must be an absolute HTTP path without whitespace",
            ));
        }
        let mut stream = std::os::unix::net::UnixStream::connect(self.socket)?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(3)))?;
        stream.set_write_timeout(Some(std::time::Duration::from_secs(3)))?;
        write!(stream, "GET {path} HTTP/1.0\r\nHost: localhost\r\n\r\n")?;
        let mut response = Vec::new();
        stream.take(MAX_RESPONSE_BYTES + 1).read_to_end(&mut response)?;
        if response.len() as u64 > MAX_RESPONSE_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("daemon response for {path} exceeds {MAX_RESPONSE_BYTES} bytes"),
            ));
        }
        let response = std::str::from_utf8(&response)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("HTTP UTF-8: {error}")))?;
        let (headers, body) = response.split_once("\r\n\r\n").ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "HTTP response has no header boundary")
        })?;
        let status = headers.lines().next().unwrap_or_default();
        let successful = status
            .split_whitespace()
            .nth(1)
            .and_then(|value| value.parse::<u16>().ok())
            .is_some_and(|status| (200..300).contains(&status));
        if !successful {
            return Err(std::io::Error::other(format!(
                "daemon request {path} returned {status:?}"
            )));
        }
        serde_json::from_str(body.trim()).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("daemon response for {path} is invalid JSON: {error}"),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::LocalHttp;
    use std::io::{Read as _, Write as _};

    struct Server {
        _directory: tempfile::TempDir,
        socket: std::path::PathBuf,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Server {
        fn new(response: &'static [u8]) -> Self {
            let directory = tempfile::tempdir().unwrap();
            let socket = directory.path().join("daemon.sock");
            let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
            let thread = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut chunk = [0_u8; 256];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let count = stream.read(&mut chunk).unwrap();
                    if count == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..count]);
                }
                // The client deliberately closes early for oversized-response tests.
                let _ = stream.write_all(response);
            });
            Self {
                _directory: directory,
                socket,
                thread: Some(thread),
            }
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.thread.take().unwrap().join().unwrap();
        }
    }

    #[test]
    fn local_http_distinguishes_content_from_transport_and_protocol_failures() {
        let valid = Server::new(b"HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n[1,2]");
        assert_eq!(LocalHttp::new(&valid.socket).get::<Vec<u8>>("/values").unwrap(), [1, 2]);

        let failed = Server::new(b"HTTP/1.0 500 Error\r\n\r\n{}");
        let error = LocalHttp::new(&failed.socket)
            .get::<serde_json::Value>("/values")
            .unwrap_err();
        assert!(error.to_string().contains("500"), "{error}");

        let malformed = Server::new(b"HTTP/1.0 200 OK\r\n\r\nnot-json");
        let error = LocalHttp::new(&malformed.socket)
            .get::<serde_json::Value>("/values")
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn local_http_rejects_oversized_responses() {
        let mut response = b"HTTP/1.0 200 OK\r\n\r\n".to_vec();
        response.resize(super::MAX_RESPONSE_BYTES as usize + 1, b' ');
        let response: &'static [u8] = Box::leak(response.into_boxed_slice());
        let server = Server::new(response);

        let error = LocalHttp::new(&server.socket)
            .get::<serde_json::Value>("/large")
            .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("exceeds"), "{error}");
    }

    #[test]
    fn local_http_rejects_request_target_injection_before_connecting() {
        let missing = std::path::Path::new("/definitely/missing/husklet.sock");
        for path in ["relative", "/valid\r\nInjected: yes", "/has space"] {
            let error = LocalHttp::new(missing).get::<serde_json::Value>(path).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput, "{path:?}");
        }
    }
}
