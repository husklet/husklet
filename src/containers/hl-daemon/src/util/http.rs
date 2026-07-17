//! Small HTTP / docker-wire helpers: log framing, the standard "no such container"
//! response, and a dependency-free base64 encoder.
use super::*;
use crate::api::ErrorMessage;

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
        Json(ErrorMessage {
            message: format!("No such container: {id}"),
        }),
    )
        .into_response()
}

/// 404 for a missing image (`docker` clients expect this exact wording).
pub(crate) fn no_such_image(name: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorMessage {
            message: format!("No such image: {name}"),
        }),
    )
        .into_response()
}

/// 404 for a missing volume.
pub(crate) fn no_such_volume(name: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorMessage {
            message: format!("no such volume: {name}"),
        }),
    )
        .into_response()
}

/// 404 for a missing network.
pub(crate) fn no_such_network(name: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorMessage {
            message: format!("no such network: {name}"),
        }),
    )
        .into_response()
}

/// 409 Conflict with a Docker-shaped `{"message": …}` body.
pub(crate) fn conflict(msg: impl Into<String>) -> Response {
    (
        StatusCode::CONFLICT,
        Json(ErrorMessage {
            message: msg.into(),
        }),
    )
        .into_response()
}

/// 400 Bad Request with a Docker-shaped `{"message": …}` body.
pub(crate) fn bad_request(msg: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorMessage {
            message: msg.into(),
        }),
    )
        .into_response()
}

/// 403 Forbidden with a Docker-shaped `{"message": …}` body.
pub(crate) fn forbidden(msg: impl Into<String>) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(ErrorMessage {
            message: msg.into(),
        }),
    )
        .into_response()
}

/// 500 Internal Server Error with a Docker-shaped `{"message": …}` body.
pub(crate) fn server_error(msg: impl Into<String>) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorMessage {
            message: msg.into(),
        }),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_frame_header_shape() {
        // `[stream, 0,0,0, len_be(4)] + payload` — the docker multiplexed-stream frame.
        let f = log_frame(1, b"hi");
        assert_eq!(f, vec![1, 0, 0, 0, 0, 0, 0, 2, b'h', b'i']);
    }

    #[test]
    fn base64_std_rfc4648_vectors() {
        // The RFC 4648 test vectors exercise all three padding cases (0/1/2 `=`).
        assert_eq!(base64_std(b""), "");
        assert_eq!(base64_std(b"f"), "Zg==");
        assert_eq!(base64_std(b"fo"), "Zm8=");
        assert_eq!(base64_std(b"foo"), "Zm9v");
        assert_eq!(base64_std(b"foob"), "Zm9vYg==");
        assert_eq!(base64_std(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_std(b"foobar"), "Zm9vYmFy");
        // a byte needing the `+`/`/` alphabet (0xFB,0xFF => "+/8=")
        assert_eq!(base64_std(&[0xfb, 0xff]), "+/8=");
    }

    #[test]
    fn log_frame_stderr_stream_id_and_len() {
        // Stream id 2 = stderr; the 4-byte big-endian length is the payload byte count.
        let payload = vec![0u8; 300];
        let f = log_frame(2, &payload);
        assert_eq!(&f[..8], &[2, 0, 0, 0, 0, 0, 1, 44]); // 300 = 0x012C
        assert_eq!(f.len(), 8 + 300);
        assert_eq!(&f[8..], &payload[..]);
    }

    #[test]
    fn log_frame_empty_payload_is_header_only() {
        assert_eq!(log_frame(1, b""), vec![1, 0, 0, 0, 0, 0, 0, 0]);
    }
}
