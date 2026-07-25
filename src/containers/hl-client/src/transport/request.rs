use super::RequestBody;
use crate::Result;
use http::{Method, Request};

pub(super) fn build(
    method: Method,
    path: &str,
    body: RequestBody,
    content_type: &'static str,
) -> Result<Request<RequestBody>> {
    Ok(Request::builder()
        .method(method)
        .uri(path)
        .header(http::header::HOST, "localhost")
        .header(http::header::CONTENT_TYPE, content_type)
        .body(body)?)
}
