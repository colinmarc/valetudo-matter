use std::pin::Pin;
use std::task::{Context, Poll};

use bstr::{BString, ByteSlice as _};
use futures::{AsyncRead, Stream};

pub(super) struct SseStream<T: AsyncRead + Unpin> {
    body: Option<T>,
    buffer: BString,
}

impl<T: AsyncRead + Unpin> SseStream<T> {
    pub(super) fn new(body: T) -> Self {
        Self {
            body: Some(body),
            buffer: BString::new(Vec::with_capacity(1024)),
        }
    }
}

impl<T: AsyncRead + Unpin> Stream for SseStream<T> {
    type Item = anyhow::Result<String>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let SseStream { body, buffer } = &mut *self;
        let Some(body_stream) = body.as_mut() else {
            return Poll::Ready(None);
        };

        let mut body_stream = Pin::new(body_stream);
        loop {
            if let Some(pos) = buffer.find("\n\n") {
                let data = buffer[..pos]
                    .lines()
                    .filter_map(|line| line.strip_prefix(b"data: "))
                    .collect::<Vec<_>>()
                    .join(&b"\n"[..]);

                *buffer = BString::from(&buffer[pos + 2..]);
                if !data.is_empty() {
                    let data = String::from_utf8(data)?;
                    return Poll::Ready(Some(Ok(data)));
                }

                continue;
            }

            let off = buffer.len();
            buffer.resize(off + 1024, 0);
            match body_stream.as_mut().poll_read(cx, &mut buffer[off..]) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Ok(0)) => {
                    body.take();
                    return Poll::Ready(None);
                }
                Poll::Ready(Ok(n)) => {
                    buffer.truncate(off + n);
                    continue;
                }
                Poll::Ready(Err(e)) => {
                    body.take();
                    return Poll::Ready(Some(Err(e.into())));
                }
            }
        }
    }
}
