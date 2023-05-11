//
// Copyright (c) 2022 chiya.dev
//
// Use of this source code is governed by the MIT License
// which can be found in the LICENSE file and at:
//
//   https://opensource.org/licenses/MIT
//
use governor::Quota;
use std::{fmt::Display, num::NonZeroU32, str::FromStr, time::Duration};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("value must be positive and follow the format \"burst/period\"")]
    Format,
}

#[derive(Debug, Clone, Copy)]
pub struct RateLimit {
    quota: Quota,
}

impl From<RateLimit> for Quota {
    fn from(limit: RateLimit) -> Self {
        limit.quota
    }
}

impl FromStr for RateLimit {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut parts = s.split('/');
        let burst = parts.next().ok_or(Error::Format)?;
        let period = parts.next().ok_or(Error::Format)?;

        parts.next().map_or(Ok(()), |_| Err(Error::Format))?;

        let burst: NonZeroU32 = burst.trim_end().parse().map_err(|_| Error::Format)?;
        let period: u64 = period.trim_start().parse().map_err(|_| Error::Format)?;

        Ok(Self {
            quota: Quota::with_period(Duration::from_secs(period) / burst.get())
                .ok_or(Error::Format)?
                .allow_burst(burst),
        })
    }
}

impl Display for RateLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{burst}/{period}",
            burst = self.quota.burst_size(),
            period = self.quota.burst_size_replenished_in().as_secs()
        )
    }
}
