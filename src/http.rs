//
// Copyright (c) 2022 chiya.dev
//
// Use of this source code is governed by the MIT License
// which can be found in the LICENSE file and at:
//
//   https://opensource.org/licenses/MIT
//
use reqwest::{Client, Proxy};

#[derive(Debug)]
pub struct HttpConfig {
    pub user_agent: Option<String>,
    pub proxy: Option<String>,
    pub compression: bool,
    pub allow_insecure: bool,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            user_agent: None,
            proxy: None,
            compression: true,
            allow_insecure: false,
        }
    }
}

impl HttpConfig {
    pub fn create_client(self) -> Result<Client, reqwest::Error> {
        let mut http = Client::builder();

        if let Some(user_agent) = self.user_agent {
            http = http.user_agent(user_agent);
        } else {
            http = http.user_agent(format!(
                "{name}/{major}.{minor}",
                name = env!("CARGO_PKG_NAME"),
                major = env!("CARGO_PKG_VERSION_MAJOR"),
                minor = env!("CARGO_PKG_VERSION_MINOR")
            ));
        }

        if let Some(proxy) = self.proxy {
            http = http.proxy(Proxy::all(proxy)?);
        }

        http.gzip(self.compression)
            .deflate(self.compression)
            .brotli(self.compression)
            .https_only(!self.allow_insecure)
            .referer(false)
            .build()
    }
}
