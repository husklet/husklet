use bytes::{Bytes, BytesMut};
use http::{Method, Request, Response, StatusCode};
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::client::conn::http1::{self, SendRequest};
use hyper_util::rt::TokioIo;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::AsyncRead;
use tokio::net::UnixStream;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::{Config, Error, Result};

mod request;
mod stream;
use request::build;
use stream::RequestBody;
pub use stream::{Stream, Upgrade};

#[derive(Debug)]
struct Connection {
    sender: SendRequest<RequestBody>,
    driver: JoinHandle<std::result::Result<(), hyper::Error>>,
}

impl Connection {
    fn abort(self) {
        self.driver.abort();
    }
}

#[derive(Debug)]
pub(crate) struct Transport {
    config: Config,
    connection: Mutex<Option<Connection>>,
}

impl Transport {
    pub(crate) fn new(config: Config) -> Self {
        Self {
            config,
            connection: Mutex::new(None),
        }
    }

    pub(crate) async fn get_unversioned(&self, path: &str) -> Result<Bytes> {
        self.request(Method::GET, path, Bytes::new()).await
    }

    pub(crate) async fn get_json_unversioned<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        Ok(serde_json::from_slice(&self.get_unversioned(path).await?)?)
    }

    pub(crate) async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let bytes = self.request(Method::GET, &self.versioned(path), Bytes::new()).await?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub(crate) async fn get(&self, path: &str) -> Result<Bytes> {
        self.request(Method::GET, &self.versioned(path), Bytes::new()).await
    }

    pub(crate) async fn json<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<T> {
        let bytes = body.map(serde_json::to_vec).transpose()?.unwrap_or_default();
        let response = self.request(method, &self.versioned(path), Bytes::from(bytes)).await?;
        Ok(serde_json::from_slice(&response)?)
    }

    pub(crate) async fn blocking_json<T: DeserializeOwned>(&self, method: Method, path: &str) -> Result<T> {
        let request = build(
            method,
            &self.versioned(path),
            RequestBody::full(Bytes::new()),
            "application/json",
        )?;
        let response = self.blocking(request).await?;
        let bytes = self.read_response(response).await?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub(crate) async fn empty(&self, method: Method, path: &str) -> Result<()> {
        self.request(method, &self.versioned(path), Bytes::new())
            .await
            .map(|_| ())
    }

    pub(crate) async fn empty_json<B: Serialize + ?Sized>(&self, method: Method, path: &str, body: &B) -> Result<()> {
        let bytes = Bytes::from(serde_json::to_vec(body)?);
        self.request(method, &self.versioned(path), bytes).await.map(|_| ())
    }

    pub(crate) async fn head(&self, path: &str) -> Result<http::HeaderMap> {
        let response = self
            .dedicated_request(
                Method::HEAD,
                &self.versioned(path),
                RequestBody::full(Bytes::new()),
                "application/json",
            )
            .await?;
        if !response.status().is_success() {
            return Err(self.read_error(response).await);
        }
        Ok(response.headers().clone())
    }

    pub(crate) async fn upload_empty<R>(&self, method: Method, path: &str, reader: R) -> Result<()>
    where
        R: AsyncRead + Send + Unpin + 'static,
    {
        let body = RequestBody::stream(reader);
        let response = self
            .dedicated_request(method, &self.versioned(path), body, "application/x-tar")
            .await?;
        self.read_response(response).await.map(|_| ())
    }

    pub(crate) async fn upload<R, T>(&self, path: &str, reader: R) -> Result<T>
    where
        R: AsyncRead + Send + Unpin + 'static,
        T: DeserializeOwned,
    {
        let body = RequestBody::stream(reader);
        let response = self
            .dedicated_request(Method::POST, &self.versioned(path), body, "application/x-tar")
            .await?;
        let bytes = self.read_response(response).await?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub(crate) async fn upload_raw<R>(&self, path: &str, reader: R) -> Result<Bytes>
    where
        R: AsyncRead + Send + Unpin + 'static,
    {
        let body = RequestBody::stream(reader);
        let response = self
            .dedicated_request(Method::POST, &self.versioned(path), body, "application/x-tar")
            .await?;
        self.read_response(response).await
    }

    pub(crate) async fn stream(&self, method: Method, path: &str) -> Result<Stream> {
        let response = self
            .dedicated_request(
                method,
                &self.versioned(path),
                RequestBody::full(Bytes::new()),
                "application/json",
            )
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(self.read_error(response).await);
        }
        let (parts, body) = response.into_parts();
        Ok(Stream {
            status: parts.status,
            headers: parts.headers,
            body,
            frame_limit: self.config.response_limit,
        })
    }

    pub(crate) async fn stream_header(
        &self,
        method: Method,
        path: &str,
        name: http::header::HeaderName,
        value: http::header::HeaderValue,
    ) -> Result<Stream> {
        let mut request = build(
            method,
            &self.versioned(path),
            RequestBody::full(Bytes::new()),
            "application/json",
        )?;
        request.headers_mut().insert(name, value);
        let response = self.dedicated(request).await?;
        if !response.status().is_success() {
            return Err(self.read_error(response).await);
        }
        let (parts, body) = response.into_parts();
        Ok(Stream {
            status: parts.status,
            headers: parts.headers,
            body,
            frame_limit: self.config.response_limit,
        })
    }

    pub(crate) async fn upgrade(&self, method: Method, path: &str) -> Result<Upgrade> {
        self.upgrade_body(method, path, Bytes::new()).await
    }

    pub(crate) async fn upgrade_json<B: Serialize + ?Sized>(
        &self,
        method: Method,
        path: &str,
        body: &B,
    ) -> Result<Upgrade> {
        self.upgrade_body(method, path, Bytes::from(serde_json::to_vec(body)?))
            .await
    }

    async fn upgrade_body(&self, method: Method, path: &str, body: Bytes) -> Result<Upgrade> {
        let path = self.versioned(path);
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header(http::header::HOST, "localhost")
            .header(http::header::CONNECTION, "Upgrade")
            .header(http::header::UPGRADE, "tcp")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(RequestBody::full(body))?;
        let mut response = self.dedicated(request).await?;
        let status = response.status();
        if status != StatusCode::SWITCHING_PROTOCOLS && !status.is_success() {
            return Err(self.read_error(response).await);
        }
        let upgraded = timeout(self.config.timeout, hyper::upgrade::on(&mut response))
            .await
            .map_err(|_| Error::Timeout)??;
        Ok(Upgrade(TokioIo::new(upgraded)))
    }

    pub(crate) fn response_limit(&self) -> usize {
        self.config.response_limit
    }

    fn versioned(&self, path: &str) -> String {
        format!("/{}{}", self.config.api_version, path)
    }

    async fn request(&self, method: Method, path: &str, body: Bytes) -> Result<Bytes> {
        let _span = hl_log::hl_span!(hl_log::tag::TRANSPORT, "docker_request");
        hl_log::hl_debug!(hl_log::tag::TRANSPORT, "docker request method={method}");
        if let Ok(result) = timeout(self.config.timeout, self.request_once(method.clone(), path, body)).await {
            result
        } else {
            hl_log::hl_warn!(hl_log::tag::TRANSPORT, "docker request timed out method={method}");
            self.invalidate().await;
            Err(Error::Timeout)
        }
    }

    async fn request_once(&self, method: Method, path: &str, body: Bytes) -> Result<Bytes> {
        let mut slot = self.connection.lock().await;
        self.prepare(&mut slot).await?;
        let request = build(method, path, RequestBody::full(body), "application/json")?;
        let response = match slot.as_mut() {
            Some(connection) => connection.sender.send_request(request).await,
            None => unreachable!("prepare installs a connection"),
        };
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                if let Some(connection) = slot.take() {
                    connection.abort();
                }
                return Err(Error::Http(error));
            }
        };
        let closes = response
            .headers()
            .get(http::header::CONNECTION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.split(',').any(|token| token.trim().eq_ignore_ascii_case("close")));
        let result = self.read_response(response).await;
        if closes || result.is_err() {
            // A cancelled or incompletely consumed HTTP/1 body cannot be reused safely.
            if let Some(connection) = slot.take() {
                connection.abort();
            }
        }
        result
    }

    async fn prepare(&self, slot: &mut Option<Connection>) -> Result<()> {
        if slot.as_ref().is_some_and(|connection| connection.driver.is_finished()) {
            return self.replace_finished(slot).await;
        }
        if slot.is_none() {
            self.connect(slot).await?;
        }
        let ready = slot.as_mut().expect("connected").sender.ready().await;
        if ready.is_err() {
            if let Some(connection) = slot.take() {
                connection.abort();
            }
            self.connect(slot).await?;
            slot.as_mut()
                .expect("reconnected")
                .sender
                .ready()
                .await
                .map_err(Error::Http)?;
        }
        Ok(())
    }

    async fn replace_finished(&self, slot: &mut Option<Connection>) -> Result<()> {
        let connection = slot.take().expect("checked before replacement");
        match connection.driver.await {
            Ok(Ok(())) => self.connect(slot).await,
            Ok(Err(error)) if error.is_closed() || error.is_incomplete_message() => {
                // A closed keep-alive socket is normal between requests; reconnect lazily.
                hl_log::hl_debug!(
                    hl_log::tag::TRANSPORT,
                    "docker keep-alive connection closed; reconnecting"
                );
                self.connect(slot).await
            }
            Ok(Err(error)) => Err(Error::Connection(error.to_string())),
            Err(error) => Err(Error::Connection(format!("connection task failed: {error}"))),
        }
    }

    async fn connect(&self, slot: &mut Option<Connection>) -> Result<()> {
        let stream = UnixStream::connect(&self.config.socket).await?;
        let (sender, connection) = http1::handshake(TokioIo::new(stream)).await?;
        let driver = tokio::spawn(connection);
        *slot = Some(Connection { sender, driver });
        Ok(())
    }

    async fn dedicated_request(
        &self,
        method: Method,
        path: &str,
        body: RequestBody,
        content_type: &'static str,
    ) -> Result<Response<Incoming>> {
        let request = build(method, path, body, content_type)?;
        self.dedicated(request).await
    }

    async fn dedicated(&self, request: Request<RequestBody>) -> Result<Response<Incoming>> {
        timeout(self.config.timeout, self.dedicated_once(request))
            .await
            .map_err(|_| Error::Timeout)?
    }

    async fn blocking(&self, request: Request<RequestBody>) -> Result<Response<Incoming>> {
        let stream = timeout(self.config.timeout, UnixStream::connect(&self.config.socket))
            .await
            .map_err(|_| Error::Timeout)??;
        let (mut sender, connection) = timeout(self.config.timeout, http1::handshake(TokioIo::new(stream)))
            .await
            .map_err(|_| Error::Timeout)??;
        tokio::spawn(async move {
            if let Err(error) = connection.await {
                hl_log::hl_debug!(
                    hl_log::tag::TRANSPORT,
                    "docker blocking connection driver failed error={error}"
                );
            }
        });
        Ok(sender.send_request(request).await?)
    }

    async fn dedicated_once(&self, request: Request<RequestBody>) -> Result<Response<Incoming>> {
        let stream = UnixStream::connect(&self.config.socket).await?;
        let (mut sender, connection) = http1::handshake(TokioIo::new(stream)).await?;
        tokio::spawn(async move {
            if let Err(error) = connection.with_upgrades().await {
                // Body and upgrade consumers receive the operational error. Keep the
                // detached driver's completion visible for transport diagnostics.
                hl_log::hl_debug!(
                    hl_log::tag::TRANSPORT,
                    "docker dedicated connection driver failed error={error}"
                );
            }
        });
        Ok(sender.send_request(request).await?)
    }

    async fn invalidate(&self) {
        let mut connection = self.connection.lock().await;
        if let Some(connection) = connection.take() {
            connection.abort();
        }
    }

    async fn read_response(&self, response: Response<Incoming>) -> Result<Bytes> {
        let status = response.status();
        let bytes = self.read_bounded(response).await?;
        if status.is_success() {
            Ok(bytes)
        } else {
            hl_log::hl_warn!(hl_log::tag::TRANSPORT, "docker request failed status={status}");
            Err(Error::docker(status, &bytes))
        }
    }

    async fn read_error(&self, response: Response<Incoming>) -> Error {
        let status = response.status();
        match self.read_bounded(response).await {
            Ok(bytes) => Error::docker(status, &bytes),
            Err(error) => error,
        }
    }

    async fn read_bounded(&self, response: Response<Incoming>) -> Result<Bytes> {
        if response
            .headers()
            .get(http::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<usize>().ok())
            .is_some_and(|length| length > self.config.response_limit)
        {
            return Err(Error::ResponseTooLarge {
                limit: self.config.response_limit,
            });
        }
        let mut body = response.into_body();
        let mut bytes = BytesMut::new();
        while let Some(frame) = body.frame().await {
            let frame = frame?;
            let Ok(data) = frame.into_data() else {
                continue;
            };
            if bytes.len().saturating_add(data.len()) > self.config.response_limit {
                return Err(Error::ResponseTooLarge {
                    limit: self.config.response_limit,
                });
            }
            bytes.extend_from_slice(&data);
        }
        Ok(bytes.freeze())
    }
}

#[cfg(test)]
mod tests;
