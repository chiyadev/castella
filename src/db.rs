//
// Copyright (c) 2022 chiya.dev
//
// Use of this source code is governed by the MIT License
// which can be found in the LICENSE file and at:
//
//   https://opensource.org/licenses/MIT
//
use self::config::DbConfigKey;
use chrono::NaiveDateTime;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sqlx::{postgres::PgPoolOptions, query, query_as, FromRow, PgPool, Postgres, Transaction};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to initialize connection pool: {0}")]
    PoolInit(sqlx::Error),

    #[error("failed to begin transaction: {0}")]
    TransactionBegin(sqlx::Error),

    #[error("failed to commit transaction: {0}")]
    TransactionCommit(sqlx::Error),

    #[error("failed to create config table: {0}")]
    ConfigTableCreate(sqlx::Error),

    #[error("failed to get config: {0}")]
    ConfigGet(sqlx::Error),

    #[error("failed to set config: {0}")]
    ConfigSet(sqlx::Error),

    #[error("failed to de/serialize config value: {0}")]
    ConfigSerde(serde_json::Error),

    #[error("failed to apply migrations: {0}")]
    Migration(sqlx::Error),

    #[error("invalid migration '{0}'; not forward compatible with that version")]
    MigrationVersionInvalid(u32),

    #[error("failed to add drive: {0}")]
    DriveAdd(sqlx::Error),

    #[error("failed to get drive: {0}")]
    DriveGet(sqlx::Error),

    #[error("failed to add file: {0}")]
    FileAdd(sqlx::Error),

    #[error("failed to get file: {0}")]
    FileGet(sqlx::Error),

    #[error("failed to delete file: {0}")]
    FileDelete(sqlx::Error),
}

#[derive(Debug, FromRow, Serialize, Deserialize)]
pub struct Drive {
    pub key: i32,
    /// Drive API shared drive resource ID.
    pub id: String,
    /// Time of drive creation.
    pub created_time: NaiveDateTime,
}

#[derive(Debug, FromRow, Serialize, Deserialize)]
pub struct File {
    pub key: i32,
    /// Drive API file resource ID.
    pub id: String,
    /// Key of the containing drive.
    pub drive_key: i32,
    /// Original size before encryption.
    pub size: i64,
    /// File content type.
    pub content_type: String,
    /// Time of file creation.
    pub created_time: NaiveDateTime,
    /// Time of last file access.
    pub accessed_time: NaiveDateTime,
    /// Encrypted file secret for decryption.
    pub secret: Vec<u8>,
}

#[derive(Debug)]
pub struct Db {
    pool: PgPool,
}

impl Db {
    pub fn new(connection: impl AsRef<str>) -> Result<Self, Error> {
        Ok(Self {
            pool: PgPoolOptions::new()
                .max_connections(10)
                .connect_lazy(connection.as_ref())
                .map_err(Error::PoolInit)?,
        })
    }

    async fn executor(&self) -> Result<DbExecutor<'_>, Error> {
        Ok(DbExecutor {
            tx: self.pool.begin().await.map_err(Error::TransactionBegin)?,
        })
    }

    pub async fn migrate(&self) -> Result<(), Error> {
        let mut exec = self.executor().await?;
        exec.migrate().await?;
        exec.commit().await
    }

    pub async fn add_drive(&self, id: impl AsRef<str>) -> Result<Drive, Error> {
        let mut exec = self.executor().await?;
        let drive = exec.add_drive(id.as_ref()).await?;
        exec.commit().await?;
        Ok(drive)
    }

    pub async fn get_drive_by_least_files(&self, max_files: u32) -> Result<Option<Drive>, Error> {
        self.executor()
            .await?
            .get_drive_by_least_files(max_files)
            .await
    }

    pub async fn add_file(
        &self,
        id: impl AsRef<str>,
        drive_key: i32,
        size: i64,
        content_type: impl AsRef<str>,
        secret: impl AsRef<[u8]>,
    ) -> Result<File, Error> {
        let mut exec = self.executor().await?;
        let file = exec
            .add_file(
                id.as_ref(),
                drive_key,
                size,
                content_type.as_ref(),
                secret.as_ref(),
            )
            .await?;
        exec.commit().await?;
        Ok(file)
    }

    pub async fn get_file_by_key(
        &self,
        key: i32,
        update_atime: bool,
    ) -> Result<Option<File>, Error> {
        let mut exec = self.executor().await?;
        let file = exec.get_file_by_key(key, update_atime).await?;
        exec.commit().await?;
        Ok(file)
    }

    pub async fn delete_file_by_key(&self, key: i32) -> Result<Option<File>, Error> {
        let mut exec = self.executor().await?;
        let file = exec.delete_file_by_key(key).await?;
        exec.commit().await?;
        Ok(file)
    }
}

#[derive(Debug)]
struct DbExecutor<'a> {
    tx: Transaction<'a, Postgres>,
}

impl DbExecutor<'_> {
    async fn commit(self) -> Result<(), Error> {
        self.tx.commit().await.map_err(Error::TransactionCommit)
    }

    async fn ensure_config_table(&mut self) -> Result<(), Error> {
        query(
            "create table if not exists config (
                key integer primary key,
                value text not null
            )",
        )
        .execute(&mut self.tx)
        .await
        .map_err(Error::ConfigTableCreate)?;

        Ok(())
    }

    async fn get_config<T: DbConfigKey>(&mut self, key: T) -> Result<Option<T::Type>, Error> {
        let value: Option<(String,)> = query_as(
            "select value from config
            where key = $1",
        )
        .bind(key.value())
        .fetch_optional(&mut self.tx)
        .await
        .map_err(Error::ConfigGet)?;

        match value {
            None => Ok(None),
            Some((value,)) => Ok(serde_json::de::from_str(&value).map_err(Error::ConfigSerde)?),
        }
    }

    async fn set_config<T: DbConfigKey>(&mut self, key: T, value: &T::Type) -> Result<(), Error> {
        query(
            "insert into config (key, value)
            values ($1, $2)
            on conflict (key)
            do
                update set value = $2",
        )
        .bind(key.value())
        .bind(serde_json::ser::to_string(value).map_err(Error::ConfigSerde)?)
        .execute(&mut self.tx)
        .await
        .map_err(Error::ConfigSet)?;

        Ok(())
    }

    async fn migrate(&mut self) -> Result<(), Error> {
        self.ensure_config_table().await?;

        let mut version = self
            .get_config(config::MigrationVersion)
            .await?
            .unwrap_or(0);

        loop {
            let queries = match version {
                0 => include_str!("sql/migration1.sql"),
                1 => break,
                _ => return Err(Error::MigrationVersionInvalid(version)),
            };

            version += 1;
            warn!("applying migration {version}");

            // sqlx hack
            for line in queries.split(';') {
                query(line)
                    .execute(&mut self.tx)
                    .await
                    .map_err(Error::Migration)?;
            }

            // let mut stream = query(queries).execute_many(&mut self.tx).await;
            // while let Some(result) = stream.next().await {
            //     result.map_err(Error::Migration)?;
            // }
        }

        self.set_config(config::MigrationVersion, &version).await?;
        Ok(())
    }

    async fn add_drive(&mut self, id: &str) -> Result<Drive, Error> {
        Ok(query_as::<_, Drive>(
            "insert into drives (id)
            values ($1)
            returning *",
        )
        .bind(id)
        .fetch_one(&mut self.tx)
        .await
        .map_err(Error::DriveAdd)?)
    }

    async fn get_drive_by_least_files(&mut self, max_files: u32) -> Result<Option<Drive>, Error> {
        Ok(query_as::<_, Drive>(
            "with counts as (
                select drive_key, count(drive_key) as count from files
                group by drive_key
                order by count asc
            )
            select drive.* from drives drive
            left join counts count on
                drive.key = count.drive_key
            where coalesce(count, 0) <= $1
            order by coalesce(count, 0) asc
            limit 1",
        )
        .bind(max_files)
        .fetch_optional(&mut self.tx)
        .await
        .map_err(Error::DriveGet)?)
    }

    async fn add_file(
        &mut self,
        id: &str,
        drive_key: i32,
        size: i64,
        content_type: &str,
        secret: &[u8],
    ) -> Result<File, Error> {
        Ok(query_as::<_, File>(
            "insert into files (id, drive_key, size, content_type, secret)
            values ($1, $2, $3, $4, $5)
            returning *",
        )
        .bind(id)
        .bind(drive_key)
        .bind(size)
        .bind(content_type)
        .bind(secret)
        .fetch_one(&mut self.tx)
        .await
        .map_err(Error::FileAdd)?)
    }

    async fn get_file_by_key(
        &mut self,
        key: i32,
        update_atime: bool,
    ) -> Result<Option<File>, Error> {
        if update_atime {
            Ok(query_as::<_, File>(
                "update files set accessed_time = timezone('utc', now())
                where key = $1
                returning *",
            )
            .bind(key)
            .fetch_optional(&mut self.tx)
            .await
            .map_err(Error::FileGet)?)
        } else {
            Ok(query_as::<_, File>(
                "select * from files
                where key = $1",
            )
            .bind(key)
            .fetch_optional(&mut self.tx)
            .await
            .map_err(Error::FileGet)?)
        }
    }

    async fn delete_file_by_key(&mut self, key: i32) -> Result<Option<File>, Error> {
        Ok(query_as::<_, File>(
            "delete from files
            where key = $1
            returning *",
        )
        .bind(key)
        .fetch_optional(&mut self.tx)
        .await
        .map_err(Error::FileDelete)?)
    }
}

mod config {
    use super::*;

    pub trait DbConfigKey {
        type Type: Serialize + DeserializeOwned;
        fn value(&self) -> u32;
    }

    macro_rules! define_key {
        ($value:expr, $name:ident, $type:ty) => {
            #[derive(Debug)]
            pub struct $name;

            impl DbConfigKey for $name {
                type Type = $type;

                fn value(&self) -> u32 {
                    $value
                }
            }
        };
    }

    define_key!(1, MigrationVersion, u32);
}
