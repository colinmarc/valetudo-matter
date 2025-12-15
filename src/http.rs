use anyhow::{Context as _, bail};
use http_body_util::{BodyStream, Empty};
use hyper::body::Bytes;
use hyper::client::conn::http1::SendRequest;
use log::error;
use smol::LocalExecutor;
use smol::net::TcpStream;
use smol::stream::{Stream, StreamExt as _};
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

    async fn connect<B>(&self, path_and_query: &str) -> anyhow::Result<(hyper::Uri, SendRequest<B>)>
    where
        B: hyper::body::Body + Send + 'static,
        B::Data: Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let mut parts = self.base.clone().into_parts();
        parts.path_and_query = Some(path_and_query.try_into()?);
        let uri = hyper::Uri::from_parts(parts)?;

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

        Ok((uri, sender))
    }

    pub(crate) async fn get<T>(&self, path_and_query: &str) -> anyhow::Result<T>
    where
        for<'t> T: serde::Deserialize<'t>,
    {
        let (uri, mut sender) = self.connect::<Empty<Bytes>>(path_and_query).await?;

        let req = hyper::Request::builder()
            .header(hyper::header::HOST, uri.authority().unwrap().as_str())
            .uri(&uri)
            .body(Empty::new())?;

        let resp = sender.send_request(req).await?;
        let status = resp.status();
        if status != 200 {
            bail!("GET {uri}: {status:?}")
        }

        let body: Vec<u8> = BodyStream::new(resp.into_body())
            .try_fold(Vec::new(), |mut body, chunk| {
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
        let (uri, mut sender) = self.connect::<String>(path_and_query).await?;

        let req = hyper::Request::builder()
            .header(hyper::header::HOST, uri.authority().unwrap().as_str())
            .uri(&uri)
            .body(body)?;

        let resp = sender.send_request(req).await?;
        let status = resp.status();
        if status != 200 {
            bail!("PUT {uri}: {status:?}")
        }

        Ok(())
    }

    pub(crate) async fn sse(
        &self,
        path_and_query: &str,
    ) -> anyhow::Result<impl Stream<Item = anyhow::Result<String>>> {
        let (uri, mut sender) = self.connect::<Empty<Bytes>>(path_and_query).await?;

        let req = hyper::Request::builder()
            .header(hyper::header::HOST, uri.authority().unwrap().as_str())
            .header(hyper::header::ACCEPT, "text/event-stream")
            .uri(&uri)
            .body(Empty::new())?;

        let resp = sender.send_request(req).await?;
        let status = resp.status();
        if status != 200 {
            bail!("GET {uri}: {status:?}")
        }

        Ok(sse::SseStream::new(resp.into_body()))
    }
}
