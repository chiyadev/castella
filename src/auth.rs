//
// Copyright (c) 2022 chiya.dev
//
// Use of this source code is governed by the MIT License
// which can be found in the LICENSE file and at:
//
//   https://opensource.org/licenses/MIT
//
use crate::http::HttpConfig;
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;
use tokio::{sync::Mutex, time::Instant};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to initialize http client: {0}")]
    ClientInit(reqwest::Error),

    #[error("failed to obtain access token: {0}")]
    AccessToken(reqwest::Error),
}

#[derive(Debug)]
pub struct Authenticator {
    http: Client,
    client_id: String,
    client_secret: String,
    refresh_token: String,
    access_token: Mutex<Option<(String, Instant)>>,
}

impl Authenticator {
    pub fn new(
        http: HttpConfig,
        client_id: String,
        client_secret: String,
        refresh_token: String,
    ) -> Result<Self, Error> {
        let http = http.create_client().map_err(Error::ClientInit)?;

        Ok(Self {
            http,
            client_id,
            client_secret,
            refresh_token,
            access_token: Mutex::new(None),
        })
    }

    pub async fn access_token(&self) -> Result<String, Error> {
        let mut value = self.access_token.lock().await;
        let expired = match *value {
            None => true,
            Some((_, expires)) => expires <= Instant::now(),
        };

        if expired {
            debug!("access token expired; obtaining a new one");
            *value = Some(self.request().await?);
        }

        Ok(value.as_ref().unwrap().0.clone())
    }

    /// Result of [access_token] formatted as `Bearer` authorization header.
    pub async fn header(&self) -> Result<String, Error> {
        Ok(format!("Bearer {}", self.access_token().await?))
    }

    async fn request(&self) -> Result<(String, Instant), Error> {
        #[derive(Deserialize)]
        struct Response {
            access_token: String,
            expires_in: u64,
        }

        let now = Instant::now();

        let Response {
            access_token,
            expires_in,
        } = self
            .http
            .post("https://oauth2.googleapis.com/token")
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", &self.client_id),
                ("client_secret", &self.client_secret),
                ("refresh_token", &self.refresh_token),
            ])
            .send()
            .await
            .and_then(|res| res.error_for_status())
            .map_err(Error::AccessToken)?
            .json()
            .await
            .map_err(Error::AccessToken)?;

        debug!("obtained access token \"{access_token}\" expiring in {expires_in} seconds");

        Ok((
            access_token,
            now + Duration::from_secs(expires_in.saturating_sub(10).max(10)), // conservative early refresh
        ))
    }
}
