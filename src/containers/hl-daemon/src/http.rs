//! HTTP request/response glue shared by the daemon's connection loop and router: the API-version
//! URI rewriting the Docker CLI expects, the image-path normalization that lets axum's single-segment
//! `:name` capture match namespaced references, and the negotiation headers stamped on every response.

use axum::{
    http::{HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

use crate::util::API_VERSION;

pub(crate) struct DockerHttp;
impl DockerHttp {
    pub(crate) fn normalize<B>(req: &mut hyper::Request<B>) {
        let mut pq = req
            .uri()
            .path_and_query()
            .map(|p| p.as_str().to_string())
            .unwrap_or_default();
        // strip the /v1.NN API-version prefix
        if let Some(rest) = pq.strip_prefix("/v1.") {
            if let Some(slash) = rest.find('/') {
                pq = rest[slash..].to_string();
            }
        }
        // collapse a NAMESPACED image reference into a single path segment so the `:name` routes match.
        // docker addresses image endpoints as /images/<ref>[/<verb>] where <ref> may embed slashes
        // (registry/ns/name:tag), e.g. POST /images/docker.io/library/ubuntu/push.
        pq = Self::image_path(&pq);
        if let Ok(uri) = pq.parse::<Uri>() {
            *req.uri_mut() = uri;
        }
    }

    /// Rewrite an image-reference path so axum's single-segment `:name` capture can match a NAMESPACED
    /// and/or tagged reference. docker addresses image endpoints as `/images/<ref>[/<verb>]` where `<ref>`
    /// may embed slashes (`registry/ns/name:tag`); `:name` only captures one segment, so we collapse the
    /// reference's internal slashes to `%2F` (axum's `Path` extractor percent-decodes them back to the full
    /// ref, and `matchit` matches `%2F` as literal chars rather than a separator). Verb sub-resources
    /// (`json`/`history`/`tag`/`push`) and the bare `DELETE /images/<ref>` are all handled; the fixed
    /// `/images/{json,create,get,load,search,prune}` endpoints carry no embedded ref and pass through.
    fn image_path(pq: &str) -> String {
        let (path, query) = pq
            .split_once('?')
            .map(|(p, q)| (p, Some(q)))
            .unwrap_or((pq, None));
        let rebuild = |p: String| match query {
            Some(q) => format!("{p}?{q}"),
            None => p,
        };
        let Some(rest) = path.strip_prefix("/images/") else {
            return pq.to_string();
        };
        let mut segs: Vec<&str> = rest.split('/').collect();
        // Peel off a trailing verb sub-resource; whatever remains is the (possibly multi-segment) reference.
        let verb = matches!(
            segs.last().copied(),
            Some("json" | "history" | "tag" | "push")
        )
        .then(|| segs.pop().unwrap());
        let reference = segs.join("/");
        // No reference means a fixed endpoint (`/images/json` list, `/images/create`, …) — leave it alone.
        if reference.is_empty() {
            return pq.to_string();
        }
        let encoded = reference.replace('/', "%2F");
        rebuild(match verb {
            Some(v) => format!("/images/{encoded}/{v}"),
            None => format!("/images/{encoded}"),
        })
    }

    pub(crate) async fn not_found(uri: Uri) -> Response {
        (
            StatusCode::NOT_FOUND,
            Json(json!({"message": format!("no route for {uri}")})),
        )
            .into_response()
    }

    /// Stamp Docker's negotiation + identity headers onto every response. The `docker` CLI reads
    /// `Api-Version`/`Ostype` from these to negotiate, and tools probe `Server`/`Docker-Experimental`.
    pub(crate) async fn headers(mut resp: Response) -> Response {
        let h = resp.headers_mut();
        h.insert("Api-Version", HeaderValue::from_static(API_VERSION));
        h.insert("Ostype", HeaderValue::from_static("linux"));
        h.insert("Docker-Experimental", HeaderValue::from_static("false"));
        h.insert("Builder-Version", HeaderValue::from_static("1"));
        h.insert(
            "Server",
            HeaderValue::from_static(concat!("hl-daemon/", env!("CARGO_PKG_VERSION"))),
        );
        h.insert(
            "Cache-Control",
            HeaderValue::from_static("no-cache, no-store, must-revalidate"),
        );
        h.insert("Pragma", HeaderValue::from_static("no-cache"));
        resp
    }
}
