//
// Copyright (c) 2022 chiya.dev
//
// Use of this source code is governed by the MIT License
// which can be found in the LICENSE file and at:
//
//   https://opensource.org/licenses/MIT
//
use crate::{
    auth::Authenticator,
    http::HttpConfig,
    rate_limit::RateLimit,
    stream::{throttle_stream, BandwidthLimiter},
};
use bytes::Bytes;
use futures::{stream::StreamExt, Stream, TryStreamExt};
use governor::{
    clock::QuantaClock,
    state::{InMemoryState, NotKeyed},
    RateLimiter,
};
use headers::{ContentLength, ContentRange, HeaderMapExt};
use http::StatusCode;
use rand::{distributions::Alphanumeric, thread_rng, Rng};
use reqwest::{Body, Client};
use serde::{Deserialize, Serialize};
use std::{ops::Range, sync::Arc};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to initialize http client: {0}")]
    ClientInit(reqwest::Error),

    #[error("failed to create file: {0}")]
    FileCreate(reqwest::Error),

    #[error("failed to serialize file metadata: {0}")]
    FileMetaSerde(serde_json::Error),

    #[error("failed to download file: {0}")]
    FileGet(reqwest::Error),

    #[error("requested file range [{0}, {1}), but response is out of bounds")]
    FileRangeResponseInvalid(u64, u64),

    #[error("failed to delete file: {0}")]
    FileDelete(reqwest::Error),

    #[error("failed to create shared drive: {0}")]
    DriveCreate(reqwest::Error),

    #[error("{0}")]
    Auth(crate::auth::Error),
}

#[derive(Debug)]
pub struct Drive {
    http: Client,
    auth: Authenticator,
    request_limiter: RateLimiter<NotKeyed, InMemoryState, QuantaClock>,
    upload_limiter: Arc<BandwidthLimiter>,
}

#[derive(Debug, Clone)]
pub struct FolderHandle {
    pub id: String,
}

impl FolderHandle {
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into() }
    }
}

#[derive(Debug, Clone)]
pub struct FileHandle {
    pub id: String,
}

impl FileHandle {
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into() }
    }
}

#[derive(Debug)]
pub struct FileResponse<S: Stream<Item = Result<Bytes, Error>>> {
    pub stream: S,
    pub range: Range<u64>,
}

impl Drive {
    pub fn new(
        http: HttpConfig,
        auth: Authenticator,
        request_limit: RateLimit,
        upload_limit: RateLimit, // MiB/s
    ) -> Result<Self, Error> {
        let http = http.create_client().map_err(Error::ClientInit)?;
        let request_limiter = RateLimiter::direct(request_limit.into());
        let upload_limiter = Arc::new(BandwidthLimiter::new(upload_limit, 1024 * 1024));

        Ok(Self {
            http,
            auth,
            request_limiter,
            upload_limiter,
        })
    }

    pub async fn create_file<S, E>(
        &self,
        name: impl AsRef<str>,
        parent: FolderHandle,
        size: u64,
        content_type: impl AsRef<str>,
        content: S,
    ) -> Result<FileHandle, Error>
    where
        S: Stream<Item = Result<Bytes, E>> + Send + Sync + 'static,
        E: std::error::Error + Send + Sync + 'static,
    {
        let name = name.as_ref();

        // reqwest doesn't support 'multipart/related' so let's build it ourselves
        // multipart boundary
        let boundary = format!(
            "----------{}",
            thread_rng()
                .sample_iter(&Alphanumeric)
                .take(50)
                .map(char::from)
                .collect::<String>()
        );

        let (body, length) = {
            #[derive(Serialize)]
            struct Request<'a, 'b> {
                name: &'a str,
                parents: [String; 1],
                #[serde(rename = "mimeType")]
                mime_type: &'b str,
            }

            // first part: json-serialized file metadata
            // second part: media content
            let meta = serde_json::ser::to_string(&Request {
                name,
                parents: [parent.id],
                mime_type: content_type.as_ref(),
            })
            .map_err(Error::FileMetaSerde)?;

            // RFC2112
            let prefix: Bytes = format!(
                "--{boundary}\r\n{meta_header}\r\n{meta}\r\n--{boundary}\r\n{media_header}\r\n",
                meta_header = "content-type: application/json; charset=utf-8\r\n",
                media_header = "content-type: application/octet-stream\r\n"
            )
            .into();
            let suffix: Bytes = format!("\r\n--{boundary}--").into();

            // construct complete body stream: prefix + media + suffix
            let concat_len = prefix.len() as u64 + size + suffix.len() as u64;
            let concat = futures::stream::once(async move { Ok(prefix) })
                .chain(content)
                .chain(futures::stream::once(async move { Ok(suffix) }));

            (concat, concat_len)
        };

        #[derive(Deserialize)]
        struct Response {
            id: String,
        }

        let body = throttle_stream(body, self.upload_limiter.clone());
        self.request_limiter.until_ready().await;

        info!("uploading new file '{name}', total size {length}");

        let Response { id } = self
            .http
            .post("https://www.googleapis.com/upload/drive/v3/files")
            .query(&[("uploadType", "multipart"), ("supportsAllDrives", "true")])
            .header(
                "authorization",
                self.auth.header().await.map_err(Error::Auth)?,
            )
            .header(
                "content-type",
                format!("multipart/related; boundary={boundary}"),
            )
            .header("content-length", length)
            .body(Body::wrap_stream(body))
            .send()
            .await
            .map_err(Error::FileCreate)?
            .error_for_status()
            .map_err(Error::FileCreate)?
            .json()
            .await
            .map_err(Error::FileCreate)?;

        info!("file '{name}' upload complete");

        Ok(FileHandle::new(id))
    }

    pub async fn get_file(
        &self,
        file: &FileHandle,
        range: Range<u64>,
    ) -> Result<FileResponse<impl Stream<Item = Result<Bytes, Error>>>, Error> {
        let FileHandle { ref id } = file;

        self.request_limiter.until_ready().await;

        debug!(
            "downloading file '{id}', range {start}-{end}",
            start = range.start,
            end = range.end
        );

        let response = self
            .http
            .get(format!("https://www.googleapis.com/drive/v3/files/{id}"))
            .query(&[
                ("alt", "media"),
                //("acknowledgeAbuse", "true"),
                ("supportsAllDrives", "true"),
            ])
            .header(
                "authorization",
                self.auth.header().await.map_err(Error::Auth)?,
            )
            .header(
                "range",
                format!(
                    "bytes={start}-{end}",
                    start = range.start,
                    end = range.end.saturating_sub(1),
                ),
            )
            .send()
            .await
            .map_err(Error::FileGet)?
            .error_for_status()
            .map_err(Error::FileGet)?;

        let response_range;

        if response.status() == StatusCode::PARTIAL_CONTENT {
            // parse content-range for partial response
            response_range = response
                .headers()
                .typed_get()
                .and_then(|range: ContentRange| range.bytes_range())
                .map(|(start, end)| start..end.saturating_add(1)) // make end exclusive
                .unwrap_or(range.clone());
        } else {
            // parse content-length and assume full response
            response_range = response
                .headers()
                .typed_get()
                .map(|len: ContentLength| 0..len.0)
                .unwrap_or(range.clone());
        }

        // ensure response range is valid
        if response_range.start > range.start || response_range.end < range.end {
            return Err(Error::FileRangeResponseInvalid(range.start, range.end));
        }

        Ok(FileResponse {
            stream: response.bytes_stream().map_err(Error::FileGet),
            range: response_range,
        })
    }

    pub async fn delete_file(&self, file: &FileHandle) -> Result<(), Error> {
        let FileHandle { ref id } = file;

        self.request_limiter.until_ready().await;
        info!("deleting file '{id}'");

        self.http
            .delete(format!("https://www.googleapis.com/drive/v3/files/{id}"))
            .query(&[("supportsAllDrives", "true")])
            .header(
                "authorization",
                self.auth.header().await.map_err(Error::Auth)?,
            )
            .send()
            .await
            .map_err(Error::FileDelete)?
            .error_for_status()
            .map_err(Error::FileDelete)?;

        Ok(())
    }

    pub async fn create_drive(&self, name: impl AsRef<str>) -> Result<FolderHandle, Error> {
        let name = name.as_ref();

        // required by drive api, we don't use it
        let request_id = thread_rng()
            .sample_iter(&Alphanumeric)
            .take(20)
            .map(char::from)
            .collect::<String>();

        #[derive(Serialize)]
        struct Request<'a> {
            name: &'a str,
            hidden: bool,
        }

        #[derive(Deserialize)]
        struct Response {
            id: String,
        }

        self.request_limiter.until_ready().await;

        info!("creating new shared drive '{name}'");

        let Response { id } = self
            .http
            .post("https://www.googleapis.com/drive/v3/drives")
            .query(&[("requestId", &request_id)])
            .header(
                "authorization",
                self.auth.header().await.map_err(Error::Auth)?,
            )
            .json(&Request { name, hidden: true })
            .send()
            .await
            .map_err(Error::DriveCreate)?
            .error_for_status()
            .map_err(Error::DriveCreate)?
            .json()
            .await
            .map_err(Error::DriveCreate)?;

        Ok(FolderHandle::new(id))
    }
}
