//! Small HTTP / docker-wire helpers: log framing, the standard "no such container"
//! response, and a dependency-free base64 encoder.
use super::*;

/// One Docker log frame: `[stream(1B), 0,0,0, len(4B big-endian)] + payload`.
pub(crate) fn log_frame(stream: u8, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + data.len());
    out.push(stream);
    out.extend_from_slice(&[0, 0, 0]);
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(data);
    out
}

pub(crate) fn no_such(id: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"message": format!("No such container: {id}")})),
    )
        .into_response()
}

/// 404 for a missing image (`docker` clients expect this exact wording).
pub(crate) fn no_such_image(name: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"message": format!("No such image: {name}")})),
    )
        .into_response()
}

/// 404 for a missing volume.
pub(crate) fn no_such_volume(name: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"message": format!("no such volume: {name}")})),
    )
        .into_response()
}

/// 404 for a missing network.
pub(crate) fn no_such_network(name: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"message": format!("no such network: {name}")})),
    )
        .into_response()
}

/// 409 Conflict with a Docker-shaped `{"message": …}` body.
pub(crate) fn conflict(msg: impl Into<String>) -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({"message": msg.into()})),
    )
        .into_response()
}

/// 400 Bad Request with a Docker-shaped `{"message": …}` body.
pub(crate) fn bad_request(msg: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"message": msg.into()})),
    )
        .into_response()
}

/// Standard base64 (no line breaks).
pub(crate) fn base64_std(data: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(A[(n >> 18 & 63) as usize] as char);
        out.push(A[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            A[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            A[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}
