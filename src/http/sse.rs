use std::pin::Pin;
use std::task::{Context, Poll};

use http_body_util::BodyStream;
use hyper::body::Incoming;
use smol::stream::{Stream, StreamExt as _};

pub(super) struct SseStream {
    body_stream: Option<BodyStream<Incoming>>,
    buffer: String,
}

impl SseStream {
    pub(super) fn new(body: Incoming) -> Self {
        Self {
            body_stream: Some(BodyStream::new(body)),
            buffer: String::new(),
        }
    }
}

impl Stream for SseStream {
    type Item = anyhow::Result<String>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let Some(body_stream) = this.body_stream.as_mut() else {
            return Poll::Ready(None);
        };

        loop {
            if let Some(pos) = this.buffer.find("\n\n") {
                let data = this.buffer[..pos]
                    .lines()
                    .filter_map(|line| line.strip_prefix("data: "))
                    .collect::<Vec<_>>()
                    .join("\n");

                this.buffer = this.buffer[pos + 2..].to_string();
                if !data.is_empty() {
                    return Poll::Ready(Some(Ok(data)));
                }

                continue;
            }

            match body_stream.poll_next(cx) {
                Poll::Ready(Some(Ok(frame))) => {
                    if let Some(chunk) = frame.data_ref() {
                        match std::str::from_utf8(chunk) {
                            Ok(s) => this.buffer.push_str(s),
                            Err(e) => {
                                this.body_stream.take();
                                return Poll::Ready(Some(Err(anyhow::anyhow!(
                                    "invalid utf-8: {e}"
                                ))));
                            }
                        }
                    }
                }
                Poll::Ready(Some(Err(e))) => {
                    this.body_stream.take();
                    return Poll::Ready(Some(Err(anyhow::anyhow!("stream error: {e}"))));
                }
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}
