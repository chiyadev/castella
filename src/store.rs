//
// Copyright (c) 2022 chiya.dev
//
// Use of this source code is governed by the MIT License
// which can be found in the LICENSE file and at:
//
//   https://opensource.org/licenses/MIT
//
use crate::{
    db::{Db, File},
    drive::{Drive, FileHandle, FileResponse, FolderHandle},
    stream::{chunk_stream, slice_stream},
};
use bytes::{Buf, Bytes};
use chacha20poly1305::{
    aead::{Aead, NewAead},
    Key, XChaCha20Poly1305, XNonce,
};
use futures::{Stream, StreamExt, TryStreamExt};
use rand::{distributions::Alphanumeric, thread_rng, Rng, RngCore};
use std::ops::{Bound, Range, RangeBounds};
use tokio::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Db(#[from] crate::db::Error),

    #[error("{0}")]
    Drive(#[from] crate::drive::Error),

    #[error("{0}")]
    Io(#[from] std::io::Error),

    #[error("invalid encryption key")]
    SecretInvalid,
}

const CHUNK_SIZE: usize = 1024 * 1024; // 1 MiB
const ENCRYPTED_CHUNK_SIZE: usize = CHUNK_SIZE + ChunkStreamCipher::TAG_SIZE;
const DRIVE_MAX_FILE_LIMIT: u32 = 350000; // conservative

#[derive(Debug)]
pub struct Store {
    db: Db,
    drive: Drive,
    file_alloc_mutex: Mutex<()>,
}

#[derive(Debug)]
pub struct FileData<S: Stream<Item = Result<Bytes, Error>>> {
    pub info: File,
    pub content: S,
    pub range: Range<u64>,
}

impl Store {
    pub fn new(db: Db, drive: Drive) -> Self {
        Self {
            db,
            drive,
            file_alloc_mutex: Mutex::new(()),
        }
    }

    fn rand_drive_name() -> String {
        format!(
            "castella-{}",
            thread_rng()
                .sample_iter(&Alphanumeric)
                .take(10)
                .map(char::from)
                .collect::<String>()
        )
    }

    fn rand_file_name() -> String {
        thread_rng()
            .sample_iter(&Alphanumeric)
            .take(20)
            .map(char::from)
            .collect()
    }

    async fn allocate_file(&self) -> Result<crate::db::Drive, Error> {
        // don't create multiple drives in race condition
        let _lock = self.file_alloc_mutex.lock().await;

        // find a drive with the least number of files and less than the limit
        match self
            .db
            .get_drive_by_least_files(DRIVE_MAX_FILE_LIMIT)
            .await?
        {
            Some(drive) => Ok(drive),
            None => {
                // such a drive doesn't exist; create a new one and add to database
                let folder = self.drive.create_drive(Self::rand_drive_name()).await?;
                Ok(self.db.add_drive(folder.id).await?)
            }
        }
    }

    pub async fn upload<S, B, E>(
        &self,
        size: u64,
        content_type: impl AsRef<str>,
        content: S,
    ) -> Result<File, Error>
    where
        S: Stream<Item = Result<B, E>> + Send + Sync + 'static,
        B: Buf + Send + Sync + 'static,
        E: std::error::Error + Send + Sync + 'static,
    {
        // allocate file to a drive
        let drive = self.allocate_file().await?;

        trace!("allocating a new file to drive '{}'", drive.id);

        // initialize cipher
        let secret = ChunkStreamCipher::gen_secret();
        let cipher = ChunkStreamCipher::new(&secret);

        // chain processing streams
        let stream = {
            let chunked = chunk_stream(size, content, CHUNK_SIZE as u64);
            let encrypted = encrypt_stream(chunked, cipher, 0);
            encrypted
        };

        // ciphertext expansion; one tag for each encrypted chunk
        let encrypted_size = size
            + (size.saturating_sub(1) / (CHUNK_SIZE as u64) + 1)
                * (ChunkStreamCipher::TAG_SIZE as u64);

        trace!("original size {size}, encrypted size {encrypted_size}");

        // upload file and add to database
        let file = self
            .drive
            .create_file(
                Self::rand_file_name(),
                FolderHandle::new(drive.id),
                encrypted_size,
                "application/octet-stream",
                stream,
            )
            .await?;

        let file = self
            .db
            .add_file(file.id, drive.key, size as i64, content_type, &*secret)
            .await?;

        Ok(file)
    }

    fn resolve_range(range: impl RangeBounds<u64>, size: u64) -> Option<Range<u64>> {
        let start = match range.start_bound() {
            Bound::Included(v) => *v,
            Bound::Excluded(v) => v.saturating_add(1),
            Bound::Unbounded => 0,
        };

        let end = match range.end_bound() {
            Bound::Included(v) => v.saturating_add(1),
            Bound::Excluded(v) => *v,
            Bound::Unbounded => size,
        };

        if start < end && end <= size {
            Some(start..end)
        } else {
            None
        }
    }

    pub async fn get(
        &self,
        key: i32,
        range: Option<impl RangeBounds<u64>>,
    ) -> Result<Option<FileData<impl Stream<Item = Result<Bytes, Error>>>>, Error> {
        // get file from database
        let file = match self.db.get_file_by_key(key, true).await? {
            Some(file) => file,
            None => return Ok(None),
        };

        // initialize cipher
        let cipher = ChunkStreamCipher::new(
            &file
                .secret
                .clone()
                .try_into()
                .map_err(|_| Error::SecretInvalid)?,
        );

        // compute ranges for decryption
        let size = file.size as u64;
        let encrypted_size = size
            + (size.saturating_sub(1) / (CHUNK_SIZE as u64) + 1)
                * (ChunkStreamCipher::TAG_SIZE as u64);

        trace!("original size {size}, encrypted size {encrypted_size}");

        let range = range
            .and_then(|range| Self::resolve_range(range, size))
            .unwrap_or(0..size);

        trace!(
            "resolved absolute range {start}-{end}",
            start = range.start,
            end = range.end
        );

        let chunk_range = {
            // ids of the chunks containing the requested range
            let start = (range.start / (CHUNK_SIZE as u64)) as u32;
            let end = (range.end.saturating_sub(1) / (CHUNK_SIZE as u64) + 1) as u32;

            start..end
        };

        trace!(
            "chunk range {start}-{end}",
            start = chunk_range.start,
            end = chunk_range.end
        );

        let encrypted_range = {
            // range to request within the encrypted file in drive
            let start = (chunk_range.start as u64) * (ENCRYPTED_CHUNK_SIZE as u64);
            let end = (chunk_range.end as u64) * (ENCRYPTED_CHUNK_SIZE as u64);

            start..end.min(encrypted_size)
        };

        let content_range = {
            // range within the decrypted stream
            // since the file is retrieved in chunks, there may be some excess unrequested
            // data at the range start and end; trim those off using this range
            let start = range.start - (chunk_range.start as u64) * (CHUNK_SIZE as u64);
            let end = start + (range.end - range.start);

            start..end
        };

        trace!(
            "content range within stream {start}-{end}",
            start = content_range.start,
            end = content_range.end
        );

        // download file from drive
        let FileResponse {
            stream,
            range: encrypted_response_range,
        } = self
            .drive
            .get_file(&FileHandle::new(file.id.clone()), encrypted_range.clone())
            .await
            .map_err(Error::Drive)?;

        // chain processing streams
        let content = {
            let (view, length) = {
                let start = encrypted_range.start - encrypted_response_range.start;
                let end = start + (encrypted_range.end - encrypted_range.start);
                (slice_stream(stream, start..end), end - start)
            };

            let chunked = chunk_stream(length, view, ENCRYPTED_CHUNK_SIZE as u64);
            let decrypted = decrypt_stream(chunked, cipher, chunk_range.start);
            let view = slice_stream(decrypted, content_range);
            view.map_err(Error::Io)
        };

        Ok(Some(FileData {
            info: file,
            content,
            range,
        }))
    }

    pub async fn get_info(&self, key: i32) -> Result<Option<File>, Error> {
        Ok(self.db.get_file_by_key(key, false).await?)
    }

    pub async fn delete(&self, key: i32) -> Result<Option<File>, Error> {
        let file = match self.db.delete_file_by_key(key).await? {
            Some(file) => file,
            None => return Ok(None),
        };

        self.drive
            .delete_file(&FileHandle::new(file.id.clone()))
            .await?;

        Ok(Some(file))
    }
}

struct ChunkStreamCipher {
    cipher: XChaCha20Poly1305,
    nonce: XNonce,
}

// aead::Error doesn't seem to implement StdError??
// this wrapper is a hack
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct CipherError(chacha20poly1305::aead::Error);

impl ChunkStreamCipher {
    const SECRET_SIZE: usize = Self::KEY_SIZE + Self::NONCE_SIZE;
    const KEY_SIZE: usize = 32;
    const NONCE_SIZE: usize = 24;
    const TAG_SIZE: usize = 16;

    pub fn gen_secret() -> Box<[u8; Self::SECRET_SIZE]> {
        let mut buffer = Box::new([0; Self::SECRET_SIZE]);
        thread_rng().fill_bytes(&mut *buffer);
        buffer
    }

    pub fn new(secret: &[u8; Self::KEY_SIZE + Self::NONCE_SIZE]) -> Self {
        let (key_part, nonce_part) = secret.split_at(Self::KEY_SIZE);
        let key = Key::from_slice(&key_part);
        let nonce = XNonce::from_slice(&nonce_part).clone();

        Self {
            cipher: XChaCha20Poly1305::new(key),
            nonce,
        }
    }

    fn get_chunk_nonce(&self, chunk_id: u32) -> XNonce {
        let mut buffer = [0; Self::NONCE_SIZE];

        // add chunk index to the last 4 bytes of nonce
        let (prefix, suffix) = self.nonce.split_at(Self::NONCE_SIZE - 4);
        let suffix = {
            let x = u32::from_be_bytes(suffix.try_into().unwrap());
            (x.wrapping_add(chunk_id)).to_be_bytes()
        };

        let (prefix2, suffix2) = buffer.split_at_mut(Self::NONCE_SIZE - 4);
        prefix2.copy_from_slice(prefix);
        suffix2.copy_from_slice(&suffix);
        buffer.into()
    }

    pub fn encrypt(&self, chunk_id: u32, chunk: &[u8]) -> Result<Vec<u8>, CipherError> {
        self.cipher
            .encrypt(&self.get_chunk_nonce(chunk_id), chunk)
            .map_err(CipherError)
    }

    pub fn decrypt(&self, chunk_id: u32, chunk: &[u8]) -> Result<Vec<u8>, CipherError> {
        self.cipher
            .decrypt(&self.get_chunk_nonce(chunk_id), chunk)
            .map_err(CipherError)
    }
}

fn encrypt_stream<S>(
    stream: S,
    cipher: ChunkStreamCipher,
    chunk_id: u32,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send + Sync + 'static
where
    S: Stream<Item = Result<Bytes, std::io::Error>> + Send + Sync + 'static,
{
    struct State<S> {
        stream: S,
        cipher: ChunkStreamCipher,
        chunk_id: u32,
    }

    futures::stream::try_unfold(
        State {
            stream: Box::pin(stream),
            cipher,
            chunk_id,
        },
        |State {
             mut stream,
             cipher,
             chunk_id,
         }| async move {
            let chunk = cipher
                .encrypt(
                    chunk_id,
                    &match stream.next().await {
                        Some(buf) => buf?,
                        None => return Ok(None),
                    },
                )
                .map_err(|err| {
                    use std::io::{Error, ErrorKind};
                    Error::new(ErrorKind::InvalidData, err)
                })?;

            trace!(
                "encrypted chunk {chunk_id} of size {size}",
                size = chunk.len() - ChunkStreamCipher::TAG_SIZE
            );

            Ok(Some((
                chunk.into(),
                State {
                    stream,
                    cipher,
                    chunk_id: chunk_id + 1,
                },
            )))
        },
    )
}

fn decrypt_stream<S>(
    stream: S,
    cipher: ChunkStreamCipher,
    chunk_id: u32,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send + Sync + 'static
where
    S: Stream<Item = Result<Bytes, std::io::Error>> + Send + Sync + 'static,
{
    struct State<S> {
        stream: S,
        cipher: ChunkStreamCipher,
        chunk_id: u32,
    }

    futures::stream::try_unfold(
        State {
            stream: Box::pin(stream),
            cipher,
            chunk_id,
        },
        |State {
             mut stream,
             cipher,
             chunk_id,
         }| async move {
            let chunk = cipher
                .decrypt(
                    chunk_id,
                    &match stream.next().await {
                        Some(buf) => buf?,
                        None => return Ok(None),
                    },
                )
                .map_err(|err| {
                    use std::io::{Error, ErrorKind};
                    Error::new(ErrorKind::InvalidData, err)
                })?;

            trace!(
                "decrypted chunk {chunk_id} of size {size}",
                size = chunk.len()
            );

            Ok(Some((
                chunk.into(),
                State {
                    stream,
                    cipher,
                    chunk_id: chunk_id + 1,
                },
            )))
        },
    )
}
