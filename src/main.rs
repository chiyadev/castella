//
// Copyright (c) 2022 chiya.dev
//
// Use of this source code is governed by the MIT License
// which can be found in the LICENSE file and at:
//
//   https://opensource.org/licenses/MIT
//
use crate::{http::HttpConfig, server::ServerConfig};
use auth::Authenticator;
use clap::Parser;
use db::Db;
use drive::Drive;
use rate_limit::RateLimit;
use server::routes;
use std::net::SocketAddr;
use store::Store;
use warp::Filter;

#[macro_use]
extern crate tracing;

mod auth;
mod db;
mod drive;
mod header;
mod http;
mod rate_limit;
mod server;
mod store;
mod stream;

#[tokio::main]
async fn main() {
    AppOptions::parse().run().await;
}

#[derive(Debug, Parser)]
#[clap(about)]
struct AppOptions {
    /// Minimum level of logs to print.
    #[clap(long, default_value = "warn", env = "CS_LOG_LEVEL")]
    log_level: String,

    /// PostgreSQL database connection string.
    #[clap(long, env = "CS_DB_CONNECTION")]
    db_connection: String,

    /// User agent string for all HTTP requests.
    #[clap(long, env = "CS_CLIENT_USER_AGENT")]
    client_user_agent: Option<String>,

    /// Proxy server through which all HTTP requests are tunneled.
    #[clap(long, env = "CS_CLIENT_PROXY")]
    client_proxy: Option<String>,

    /// Allow sending requests on insecure HTTP connections.
    #[clap(long, env = "CS_CLIENT_ALLOW_INSECURE")]
    client_allow_insecure: bool,

    /// Google OAuth2 client ID.
    #[clap(long, env = "CS_OAUTH_CLIENT_ID")]
    oauth_client_id: String,

    /// Google OAuth2 client secret.
    #[clap(long, env = "CS_OAUTH_CLIENT_SECRET")]
    oauth_client_secret: String,

    /// Google OAuth2 refresh token.
    #[clap(long, env = "CS_OAUTH_REFRESH_TOKEN")]
    oauth_refresh_token: String,

    /// Rate limit for all Drive API requests, measured in requests/s.
    #[clap(long, default_value = "10000/100", env = "CS_DRIVE_REQUEST_LIMIT")]
    drive_request_limit: RateLimit,

    /// Bandwidth limit for all Drive API upload requests, measured in MiB/s.
    #[clap(long, default_value = "700000/86400", env = "CS_DRIVE_UPLOAD_LIMIT")]
    drive_upload_limit: RateLimit,

    /// Local socket address on which requests will be listened.
    #[clap(long, default_value = "127.0.0.1:1707", env = "CS_SERVER_ENDPOINT")]
    server_endpoint: SocketAddr,

    /// Maximum body size of a single upload request, measured in MiB.
    #[clap(long, default_value = "102400", env = "CS_SERVER_MAX_UPLOAD_SIZE")]
    server_max_upload_size: u64,
}

impl AppOptions {
    pub async fn run(self) {
        // initialize logger
        tracing_subscriber::fmt()
            .with_env_filter(&self.log_level)
            .init();

        debug!("parsed options: {:?}", self);

        let Self {
            log_level: _,
            db_connection,
            client_user_agent,
            client_proxy,
            client_allow_insecure,
            oauth_client_id,
            oauth_client_secret,
            oauth_refresh_token,
            drive_request_limit,
            drive_upload_limit,
            server_endpoint,
            server_max_upload_size,
        } = self;

        // drive authenticator
        let auth = Authenticator::new(
            HttpConfig {
                user_agent: client_user_agent.clone(),
                proxy: client_proxy.clone(),
                compression: true,
                allow_insecure: client_allow_insecure,
            },
            oauth_client_id,
            oauth_client_secret,
            oauth_refresh_token,
        )
        .expect("failed to initialize oauth client");

        // drive client
        let drive = Drive::new(
            HttpConfig {
                user_agent: client_user_agent,
                proxy: client_proxy,
                compression: false, // don't try to compress encrypted data
                allow_insecure: client_allow_insecure,
            },
            auth,
            drive_request_limit,
            drive_upload_limit,
        )
        .expect("failed to initialize drive client");

        debug!("connecting to database");

        // database client
        let db = Db::new(db_connection).expect("failed to initialize database client");
        db.migrate().await.expect("failed to migrate database");

        info!("initialization complete; starting http server");

        // frontend server
        warp::serve(
            routes(ServerConfig {
                store: Store::new(db, drive).into(),
                max_upload_size: server_max_upload_size * 1024 * 1024, // MiB to B
            })
            .with(warp::log("warp")),
        )
        .run(server_endpoint)
        .await;
    }
}
