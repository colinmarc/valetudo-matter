use std::io;

use anyhow::{Context as _, bail};
use async_compression::futures::bufread::GzipDecoder;
use futures::{TryStreamExt as _, io::BufReader};
use http_body_util::{BodyExt, BodyStream, Empty};
use hyper::{body::Bytes, client::conn::http1::SendRequest};
use log::error;
use smol::{
    LocalExecutor,
    net::TcpStream,
    stream::{Stream, StreamExt as _},
};
use smol_hyper::rt::FuturesIo;

mod sse;

#[derive(Clone)]
pub(crate) struct ValetudoClient {
    executor: &'static LocalExecutor<'static>,
    base: hyper::Uri,
}

impl ValetudoClient {
    pub(crate) fn new(executor: &'static LocalExecutor<'static>, base: hyper::Uri) -> Self {
        Self { executor, base }
    }

    async fn connect<B>(&self, uri: &hyper::Uri) -> anyhow::Result<SendRequest<B>>
    where
        B: hyper::body::Body + Send + 'static,
        B::Data: Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let host = uri.host().context("cannot parse host")?;
        let port = uri.port_u16().unwrap_or(80);
        let io = TcpStream::connect((host, port)).await?;

        let (sender, conn) = hyper::client::conn::http1::handshake(FuturesIo::new(io)).await?;
        self.executor
            .spawn(async move {
                if let Err(e) = conn.await {
                    error!("failed to connect to valetudo: {e:?}")
                }
            })
            .detach();

        Ok(sender)
    }

    pub(crate) async fn get<T>(&self, path_and_query: &str) -> anyhow::Result<T>
    where
        for<'t> T: serde::Deserialize<'t>,
    {
        let uri = self.uri(path_and_query)?;
        let mut sender = self.connect::<Empty<Bytes>>(&uri).await?;

        let req = hyper::Request::builder()
            .method(hyper::Method::GET)
            .header(hyper::header::HOST, uri.authority().unwrap().as_str())
            .header(hyper::header::ACCEPT, "application/json")
            .header(hyper::header::CONNECTION, "close")
            .uri(&uri)
            .body(Empty::new())?;

        let resp = sender.send_request(req).await?;
        let status = resp.status();
        if status != 200 {
            bail!("GET {uri}: {status:?}")
        }

        let body: Vec<u8> = BodyStream::new(resp.into_body())
            .try_fold(Vec::new(), |mut body, chunk| async move {
                if let Some(chunk) = chunk.data_ref() {
                    body.extend_from_slice(chunk);
                }
                Ok(body)
            })
            .await?;

        let res: T = serde_json::from_slice(&body).context("Invalid response")?;
        Ok(res)
    }

    pub(crate) async fn put(&self, path_and_query: &str, body: String) -> anyhow::Result<()> {
        let uri = self.uri(path_and_query)?;
        let mut sender = self.connect::<String>(&uri).await?;

        let req = hyper::Request::builder()
            .method(hyper::Method::PUT)
            .header(hyper::header::HOST, uri.authority().unwrap().as_str())
            .header(hyper::header::CONTENT_TYPE, "application/json")
            .header(hyper::header::CONNECTION, "close")
            .header(hyper::header::ACCEPT_ENCODING, "identity")
            .uri(&uri)
            .body(body)?;

        let resp = sender.send_request(req).await?;
        let status = resp.status();
        if status != 200 {
            bail!("PUT {uri}: {status:?}")
        }

        // Consume the response body.
        let _ = BodyStream::new(resp.into_body()).for_each(|_| ()).await;

        Ok(())
    }

    pub(crate) async fn sse(
        &self,
        path_and_query: &str,
    ) -> anyhow::Result<impl Stream<Item = anyhow::Result<String>>> {
        let uri = self.uri(path_and_query)?;
        let mut sender = self.connect::<Empty<Bytes>>(&uri).await?;

        let req = hyper::Request::builder()
            .method(hyper::Method::GET)
            .header(hyper::header::HOST, uri.authority().unwrap().as_str())
            .header(hyper::header::ACCEPT, "text/event-stream")
            // Valetudo has a bug where SSE only works with gzip.
            .header(hyper::header::ACCEPT_ENCODING, "gzip")
            .header(hyper::header::CONNECTION, "keep-alive")
            .uri(&uri)
            .body(Empty::new())?;

        let resp = sender.send_request(req).await?;
        let status = resp.status();
        if status != 200 {
            bail!("GET {uri}: {status:?}")
        }

        let encoding = resp.headers().get(hyper::header::CONTENT_ENCODING).cloned();
        let body = BufReader::new(
            resp.into_body()
                .into_data_stream()
                .map_err(io::Error::other)
                .into_async_read(),
        );

        match encoding.as_ref().map(|v| v.to_str()) {
            None | Some(Ok("gzip")) => {
                let decoder = GzipDecoder::new(body);
                Ok(sse::SseStream::new(decoder))
            }
            Some(v) => bail!("Unexpected content-encoding: {}", v?),
        }
    }

    fn uri(&self, path_and_query: &str) -> anyhow::Result<hyper::Uri> {
        let mut parts = self.base.clone().into_parts();
        parts.path_and_query = Some(path_and_query.try_into()?);
        let uri = hyper::Uri::from_parts(parts)?;
        Ok(uri)
    }
}
