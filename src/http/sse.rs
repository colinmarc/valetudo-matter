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

            // Pending is not data: never extend the accumulated event until a read succeeds.
            let mut chunk = [0u8; 1024];
            match body_stream.as_mut().poll_read(cx, &mut chunk) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Ok(0)) => {
                    body.take();
                    return Poll::Ready(None);
                }
                Poll::Ready(Ok(n)) => {
                    buffer.extend_from_slice(&chunk[..n]);
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

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::collections::VecDeque;
    struct Chunks(VecDeque<Option<&'static [u8]>>);
    impl AsyncRead for Chunks {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut [u8],
        ) -> Poll<std::io::Result<usize>> {
            match self.0.pop_front() {
                Some(None) => {
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
                Some(Some(bytes)) => {
                    buf[..bytes.len()].copy_from_slice(bytes);
                    Poll::Ready(Ok(bytes.len()))
                }
                None => Poll::Ready(Ok(0)),
            }
        }
    }
    #[test]
    fn pending_reads_do_not_corrupt_returning_then_docked_events() {
        let chunks = Chunks(VecDeque::from([
            None,
            Some(&b"data: {\"value\":\"return"[..]),
            None,
            Some(&b"ing\"}\n\n"[..]),
            None,
            Some(&b"data: {\"value\":\"docked\"}\n\n"[..]),
        ]));
        let events = smol::block_on(SseStream::new(chunks).collect::<Vec<_>>());
        let events = events
            .into_iter()
            .collect::<anyhow::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            events,
            vec![r#"{"value":"returning"}"#, r#"{"value":"docked"}"#]
        );
    }
}
