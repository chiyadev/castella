//
// Copyright (c) 2022 chiya.dev
//
// Use of this source code is governed by the MIT License
// which can be found in the LICENSE file and at:
//
//   https://opensource.org/licenses/MIT
//
use crate::{
    db::File,
    header::parse_single_range_header,
    store::{FileData, Store},
};
use bytes::Buf;
use chrono::{DateTime, Utc};
use futures::Stream;
use http::StatusCode;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{convert::Infallible, num::NonZeroU64, sync::Arc};
use warp::{
    any, body, delete, filters::BoxedFilter, get, head, header, hyper, path, post, reject, reply,
    Filter, Rejection, Reply,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Store(#[from] crate::store::Error),

    #[error("no such file")]
    FileNotExists,
}

#[derive(Debug)]
pub struct ServerConfig {
    pub store: Arc<Store>,
    pub max_upload_size: u64,
}

pub fn routes(config: ServerConfig) -> BoxedFilter<(impl Reply,)> {
    let ServerConfig {
        store,
        max_upload_size,
    } = config;

    let store = any().map(move || store.clone());
    let get_root = get().and(path!()).map(get_root).boxed();

    // HEAD /$id
    let head_file = head()
        .and(path!(i32))
        .and(store.clone())
        .then(head_file)
        .map(handle_result)
        .boxed();

    // GET /$id
    let get_file = get()
        .and(path!(i32))
        .and(store.clone())
        .and(header::optional("range"))
        .then(get_file)
        .map(handle_result)
        .boxed();

    // POST /
    let upload_file = post()
        .and(path!())
        .and(body::content_length_limit(max_upload_size))
        .and(store.clone())
        .and(header("content-length"))
        .and(header::optional("content-type"))
        .and(body::stream())
        .then(upload_file)
        .map(handle_result)
        .boxed();

    // DELETE /$id
    let delete_file = delete()
        .and(path!(i32))
        .and(store.clone())
        .then(delete_file)
        .map(handle_result)
        .boxed();

    let routes = get_root
        .or(get_file)
        .or(head_file)
        .or(upload_file)
        .or(delete_file);

    routes
        .map(|reply| reply::with_header(reply, "server", "castella"))
        .recover(recover)
        .boxed()
}

fn get_root() -> impl Reply {
    "castella file server"
}

const FILE_CACHE_CONTROL: &str = "public,max-age=31536000,immutable";

fn get_file_etag(file: &File) -> String {
    base64::encode_config(Sha256::digest(&file.id), base64::URL_SAFE_NO_PAD)
}

fn add_file_headers(reply: impl Reply, file: &File, length: u64) -> impl Reply {
    reply::with_header(
        reply::with_header(
            reply::with_header(
                reply::with_header(
                    reply::with_header(
                        reply::with_header(reply, "content-type", &file.content_type),
                        "content-length",
                        length,
                    ),
                    "cache-control",
                    FILE_CACHE_CONTROL,
                ),
                "last-modified",
                DateTime::<Utc>::from_utc(file.created_time, Utc).to_rfc2822(),
            ),
            "etag",
            format!("\"{}\"", get_file_etag(file)),
        ),
        "accept-ranges",
        "bytes",
    )
}

async fn head_file(key: i32, store: Arc<Store>) -> Result<impl Reply, Error> {
    let file = store.get_info(key).await?.ok_or(Error::FileNotExists)?;
    let size = file.size as u64;

    Ok(add_file_headers(reply(), &file, size))
}

async fn get_file(key: i32, store: Arc<Store>, range: Option<String>) -> Result<impl Reply, Error> {
    let FileData {
        info: file,
        content,
        range,
    } = store
        .get(key, range.and_then(parse_single_range_header))
        .await?
        .ok_or(Error::FileNotExists)?;

    let size = file.size as u64;
    let range_length = range.end - range.start;

    let res = add_file_headers(
        reply::Response::new(hyper::Body::wrap_stream(content)),
        &file,
        range_length,
    );

    let res = if range_length == size {
        res.into_response()
    } else {
        reply::with_header(
            reply::with_status(res, StatusCode::PARTIAL_CONTENT),
            "content-range",
            format!(
                "bytes {start}-{end}/{size}",
                start = range.start,
                end = range.end.saturating_sub(1)
            ),
        )
        .into_response()
    };

    Ok(res)
}

async fn upload_file<S, B>(
    store: Arc<Store>,
    size: NonZeroU64,
    content_type: Option<String>,
    content: S,
) -> Result<impl Reply, Error>
where
    S: Stream<Item = Result<B, warp::Error>> + Send + Sync + 'static,
    B: Buf + Send + Sync + 'static,
{
    let content_type = content_type
        .as_deref()
        .unwrap_or("application/octet-stream");

    let File {
        key,
        size,
        content_type,
        created_time,
        ..
    } = store.upload(size.get(), content_type, content).await?;

    #[derive(Serialize)]
    struct Response {
        key: i32,
        size: i64,
        content_type: String,
        created_time: DateTime<Utc>,
    }

    Ok(reply::json(&Response {
        key,
        size,
        content_type,
        created_time: DateTime::from_utc(created_time, Utc),
    }))
}

async fn delete_file(key: i32, store: Arc<Store>) -> Result<impl Reply, Error> {
    store.delete(key).await?.ok_or(Error::FileNotExists)?;

    #[derive(Serialize)]
    struct Response {
        deleted: bool,
    }

    Ok(reply::json(&Response { deleted: true }))
}

fn handle_result(result: Result<impl Reply, Error>) -> impl Reply {
    match result {
        Ok(reply) => reply.into_response(),
        Err(err) => reply_error(
            match err {
                Error::Store(ref err) => {
                    warn!("{err}");
                    StatusCode::INTERNAL_SERVER_ERROR
                }
                Error::FileNotExists => StatusCode::NOT_FOUND,
            },
            err.to_string(),
        )
        .into_response(),
    }
}

async fn recover(err: Rejection) -> Result<impl Reply, Infallible> {
    Ok(if err.is_not_found() {
        reply_error(StatusCode::NOT_FOUND, "not found")
    } else if let Some(err) = err.find::<reject::InvalidHeader>() {
        reply_error(
            StatusCode::BAD_REQUEST,
            format!("invalid {} header", err.name()),
        )
    } else if let Some(err) = err.find::<reject::MissingHeader>() {
        reply_error(
            StatusCode::BAD_REQUEST,
            format!("missing {} header", err.name()),
        )
    } else if let Some(_) = err.find::<reject::LengthRequired>() {
        reply_error(StatusCode::BAD_REQUEST, "missing content-length header")
    } else if let Some(_) = err.find::<reject::InvalidQuery>() {
        reply_error(StatusCode::BAD_REQUEST, "invalid query string")
    } else if let Some(_) = err.find::<reject::MethodNotAllowed>() {
        reply_error(StatusCode::BAD_REQUEST, "method not allowed")
    } else if let Some(_) = err.find::<reject::PayloadTooLarge>() {
        reply_error(StatusCode::PAYLOAD_TOO_LARGE, "request body too large")
    } else if let Some(_) = err.find::<reject::UnsupportedMediaType>() {
        reply_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported content-type",
        )
    } else {
        warn!("unknown rejection: {err:?}");
        reply_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("unknown error: {err:?}"),
        )
    })
}

fn reply_error(status: StatusCode, message: impl Into<String>) -> reply::Response {
    #[derive(Serialize)]
    struct Error {
        error: bool,
        status: u16,
        message: String,
    }

    reply::with_status(
        reply::json(&Error {
            error: true,
            status: status.as_u16(),
            message: message.into(),
        }),
        status,
    )
    .into_response()
}
