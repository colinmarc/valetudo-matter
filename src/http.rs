use std::io;

use anyhow::{Context as _, bail};
use async_compression::futures::bufread::GzipDecoder;
use futures::{TryStreamExt as _, io::BufReader};
use http_body_util::{BodyExt, BodyStream, Empty};
use hyper::{body::Bytes, client::conn::http1::SendRequest};
use log::error;
use smol::{LocalExecutor, net::TcpStream, stream::Stream};
use smol_hyper::rt::FuturesIo;

mod sse;

#[derive(Clone)]
pub(crate) struct ValetudoClient {
    executor: &'static LocalExecutor<'static>,
    base: hyper::Uri,
    request_timeout: std::time::Duration,
}

impl ValetudoClient {
    pub(crate) fn new(executor: &'static LocalExecutor<'static>, base: hyper::Uri) -> Self {
        Self {
            executor,
            base,
            request_timeout: std::time::Duration::from_secs(6),
        }
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
        smol::future::or(self.get_once(path_and_query), async {
            smol::Timer::after(self.request_timeout).await;
            bail!("GET {path_and_query} timed out");
        })
        .await
    }

    async fn get_once<T>(&self, path_and_query: &str) -> anyhow::Result<T>
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

    pub(crate) async fn get_optional<T>(&self, path: &str) -> anyhow::Result<T>
    where
        for<'t> T: serde::Deserialize<'t>,
    {
        self.get(path).await
    }

    pub(crate) async fn put(&self, path_and_query: &str, body: String) -> anyhow::Result<()> {
        // One deadline covers DNS, connect, headers and the complete response.
        // Never retry a mutating request: a timeout may mean it was executed.
        smol::future::or(self.put_once(path_and_query, body), async {
            smol::Timer::after(self.request_timeout).await;
            bail!("PUT {path_and_query} timed out; command outcome is unknown");
        })
        .await
    }

    async fn put_once(&self, path_and_query: &str, body: String) -> anyhow::Result<()> {
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
        BodyStream::new(resp.into_body())
            .try_for_each(|_| async { Ok(()) })
            .await?;

        Ok(())
    }

    pub(crate) async fn sse(
        &self,
        path_and_query: &str,
    ) -> anyhow::Result<impl Stream<Item = anyhow::Result<String>>> {
        // Bound connection establishment only. An established SSE body is
        // intentionally long lived; its liveness is reconciled by state polling.
        smol::future::or(self.sse_connect(path_and_query), async {
            smol::Timer::after(self.request_timeout).await;
            bail!("SSE {path_and_query} handshake timed out");
        })
        .await
    }

    async fn sse_connect(
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    #[test]
    fn get_and_sse_handshake_have_deadlines_but_sse_body_is_long_lived() {
        // 0 GET headers, 1 GET body, 2 SSE headers, 3 established SSE.
        for case in 0..4 {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let (release, wait) = std::sync::mpsc::channel();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                if case == 1 {
                    stream
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\n\r\n{")
                        .unwrap();
                }
                if case == 3 {
                    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nContent-Encoding: gzip\r\n\r\n").unwrap();
                }
                wait.recv_timeout(std::time::Duration::from_secs(3))
                    .unwrap();
            });
            let executor = Box::leak(Box::new(LocalExecutor::new()));
            smol::block_on(executor.run(async {
                let mut client =
                    ValetudoClient::new(executor, format!("http://{address}").parse().unwrap());
                client.request_timeout = std::time::Duration::from_millis(100);
                if case < 2 {
                    let error = client.get::<serde_json::Value>("/state").await.unwrap_err();
                    assert!(error.to_string().contains("timed out"));
                } else if case == 2 {
                    let result = client.sse("/state/sse").await;
                    assert!(
                        result
                            .err()
                            .unwrap()
                            .to_string()
                            .contains("handshake timed out")
                    );
                } else {
                    let _stream = client.sse("/state/sse").await.unwrap();
                    smol::Timer::after(std::time::Duration::from_millis(150)).await;
                    // The established stream owns the response body past the
                    // connection deadline, without requiring a first event.
                }
            }));
            release.send(()).unwrap();
            server.join().unwrap();
        }
    }
    #[test]
    fn put_deadline_covers_stalled_headers_and_body_without_retry() {
        for partial_response in [false, true] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let (release, wait) = std::sync::mpsc::channel();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                if partial_response {
                    stream
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nx")
                        .unwrap();
                }
                wait.recv_timeout(std::time::Duration::from_secs(3))
                    .unwrap();
                listener.set_nonblocking(true).unwrap();
                assert!(
                    matches!(listener.accept(), Err(e) if e.kind() == io::ErrorKind::WouldBlock)
                );
            });
            let executor = Box::leak(Box::new(LocalExecutor::new()));
            smol::block_on(executor.run(async {
                let mut client =
                    ValetudoClient::new(executor, format!("http://{address}").parse().unwrap());
                client.request_timeout = std::time::Duration::from_millis(100);
                let error = client.put("/control", "{}".into()).await.unwrap_err();
                assert!(error.to_string().contains("outcome is unknown"));
            }));
            release.send(()).unwrap();
            server.join().unwrap();
        }
    }
}
