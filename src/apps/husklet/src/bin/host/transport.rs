use std::io::{Read, Write};

pub struct LocalHttp<'a> {
    socket: &'a std::path::Path,
}

impl<'a> LocalHttp<'a> {
    pub fn new(socket: &'a std::path::Path) -> Self {
        Self { socket }
    }

    pub fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Option<T> {
        let mut stream = std::os::unix::net::UnixStream::connect(self.socket).ok()?;
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(3)));
        write!(stream, "GET {path} HTTP/1.0\r\nHost: localhost\r\n\r\n").ok()?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response).ok()?;
        let response = String::from_utf8_lossy(&response);
        let body = response.split("\r\n\r\n").nth(1)?;
        serde_json::from_str(body.trim()).ok()
    }
}
