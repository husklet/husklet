//! Network probes shared by daemon integration tests.

use crate::api::support::TIMEOUT;
use tokio::{io::AsyncReadExt, time::timeout};

pub(crate) async fn published(
    address: std::net::Ipv4Addr,
    port: u16,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut stream = timeout(TIMEOUT, tokio::net::TcpStream::connect((address, port))).await??;
    let mut bytes = Vec::new();
    timeout(TIMEOUT, stream.read_to_end(&mut bytes)).await??;
    Ok(bytes)
}
