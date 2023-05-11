//
// Copyright (c) 2022 chiya.dev
//
// Use of this source code is governed by the MIT License
// which can be found in the LICENSE file and at:
//
//   https://opensource.org/licenses/MIT
//
use crate::rate_limit::RateLimit;
use bytes::{Buf, Bytes};
use futures::{Stream, StreamExt, TryStreamExt};
use governor::{
    clock::QuantaClock,
    state::{InMemoryState, NotKeyed},
    RateLimiter,
};
use std::{
    ops::Range,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};
use tokio::io::AsyncReadExt;
use tokio_util::io::StreamReader;

pub fn slice_stream<S, E>(
    stream: S,
    range: Range<u64>,
) -> impl Stream<Item = Result<Bytes, E>> + Send + Sync + 'static
where
    S: Stream<Item = Result<Bytes, E>> + Send + Sync + 'static,
    E: std::error::Error + Send + Sync + 'static,
{
    struct State<S> {
        stream: S,
        offset: u64,
        limit: u64,
    }

    futures::stream::try_unfold(
        State {
            stream: Box::pin(stream),
            offset: range.start,
            limit: range.end - range.start,
        },
        |State {
             mut stream,
             mut offset,
             mut limit,
         }| async move {
            while offset != 0 {
                let mut chunk = match stream.next().await {
                    Some(buf) => buf?,
                    None => return Ok(None),
                };

                let size = chunk.len() as u64;
                if size <= offset {
                    offset -= size;
                } else {
                    return Ok(Some((
                        chunk.split_off(offset as usize),
                        State {
                            stream,
                            offset: 0,
                            limit,
                        },
                    )));
                }
            }

            if limit == 0 {
                Ok(None)
            } else {
                let mut chunk = match stream.next().await {
                    Some(buf) => buf?,
                    None => return Ok(None),
                };

                let take = limit.min(chunk.len() as u64);
                limit -= take;

                Ok(Some((
                    chunk.split_to(take as usize),
                    State {
                        stream,
                        offset,
                        limit,
                    },
                )))
            }
        },
    )
}

pub fn chunk_stream<S, B, E>(
    size: u64,
    stream: S,
    chunk_size: u64,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send + Sync + 'static
where
    S: Stream<Item = Result<B, E>> + Send + Sync + 'static,
    B: Buf + Send + Sync + 'static,
    E: std::error::Error + Send + Sync + 'static,
{
    // futures IntoAsyncRead doesn't support reading from Buf,
    // so let's use tokio-util StreamReader instead.
    let reader = StreamReader::new(stream.map_err(|err| {
        use std::io::{Error, ErrorKind};
        Error::new(ErrorKind::Other, err)
    }));

    struct State<R> {
        reader: R,
        chunk_size: u64,
        remaining: u64,
    }

    futures::stream::try_unfold(
        State {
            reader: Box::pin(reader),
            chunk_size,
            remaining: size,
        },
        |State {
             mut reader,
             chunk_size,
             remaining,
         }| async move {
            // read chunk_size or whatever is remaining in the stream, whichever is lesser
            let read = remaining.min(chunk_size) as usize;

            if read == 0 {
                Ok(None)
            } else {
                let mut buffer = vec![0; read];
                reader.read_exact(&mut buffer).await?;

                Ok(Some((
                    buffer.into(),
                    State {
                        reader,
                        chunk_size,
                        remaining: remaining - read as u64,
                    },
                )))
            }
        },
    )
}

#[derive(Debug)]
pub struct BandwidthLimiter {
    // unit of measurement for the limiter.
    // this is necessary because RateLimiter takes NonZeroU32 but we need to support more than 4 GB traffic...
    // e.g. if limiter is measured in MiB/s, then unit is 1024*1024 and one cell is consumed from the limiter per mebibyte.
    unit: u64,
    limiter: RateLimiter<NotKeyed, InMemoryState, QuantaClock>,
    leftover: AtomicU64,
}

impl BandwidthLimiter {
    pub fn new(limit: RateLimit, unit: u64) -> Self {
        Self {
            unit,
            limiter: RateLimiter::direct(limit.into()),
            leftover: AtomicU64::new(0),
        }
    }

    async fn throttle(&self, buffer: &[u8]) {
        let length = buffer.len() as u64;
        let mut old = self.leftover.load(Ordering::Relaxed);

        let consume = loop {
            let consume = ((old + length) / self.unit) as u32;
            let new = (old + length) % self.unit;

            match self
                .leftover
                .compare_exchange_weak(old, new, Ordering::SeqCst, Ordering::Relaxed)
            {
                Ok(_) => break consume,
                Err(v) => old = v,
            }
        };

        // fails if consume is zero
        if let Ok(consume) = consume.try_into() {
            trace!(
                "throttling stream, consuming {consume} cell(s) from limiter (scale={unit})",
                unit = self.unit
            );

            let _ = self.limiter.until_n_ready(consume).await;
        }
    }
}

pub fn throttle_stream<S, E>(
    stream: S,
    limiter: Arc<BandwidthLimiter>,
) -> impl Stream<Item = Result<Bytes, E>>
where
    S: Stream<Item = Result<Bytes, E>>,
    E: std::error::Error,
{
    struct State<S, T> {
        stream: S,
        limiter: T,
    }

    futures::stream::try_unfold(
        State {
            stream: Box::pin(stream),
            limiter,
        },
        |State {
             mut stream,
             limiter,
         }| async move {
            let buffer = match stream.next().await {
                Some(buf) => buf?,
                None => return Ok(None),
            };

            limiter.throttle(&buffer).await;

            Ok(Some((buffer, State { stream, limiter })))
        },
    )
}

#[allow(dead_code)]
pub fn debug_stream<S, E>(stream: S) -> impl Stream<Item = Result<Bytes, E>>
where
    S: Stream<Item = Result<Bytes, E>>,
    E: std::error::Error,
{
    futures::stream::try_unfold(Box::pin(stream), |mut stream| async move {
        let buffer = match stream.next().await {
            Some(buf) => buf?,
            None => return Ok(None),
        };

        trace!("sending {} bytes", buffer.len());
        Ok(Some((buffer, stream)))
    })
}
