//
// Copyright (c) 2022 chiya.dev
//
// Use of this source code is governed by the MIT License
// which can be found in the LICENSE file and at:
//
//   https://opensource.org/licenses/MIT
//
use std::ops::{Bound, RangeBounds};

// nightly only:
/// https://doc.rust-lang.org/std/ops/enum.Bound.html#method.as_ref
fn bound_as_ref<T>(bound: &Bound<T>) -> Bound<&T> {
    match bound {
        Bound::Included(ref x) => Bound::Included(x),
        Bound::Excluded(ref x) => Bound::Excluded(x),
        Bound::Unbounded => Bound::Unbounded,
    }
}

#[derive(Debug, Clone)]
struct RangeCustom<T> {
    start: Bound<T>,
    end: Bound<T>,
}

impl<T> RangeBounds<T> for RangeCustom<T> {
    fn start_bound(&self) -> Bound<&T> {
        bound_as_ref(&self.start)
    }

    fn end_bound(&self) -> Bound<&T> {
        bound_as_ref(&self.end)
    }
}

pub fn parse_single_range_header(s: impl AsRef<str>) -> Option<impl RangeBounds<u64>> {
    let s = s.as_ref();
    if !s.starts_with("bytes=") {
        return None;
    }

    let s = &s["bytes=".len()..];
    let (start, end) = s.split_once('-')?;

    let start = if start == "" {
        Bound::Unbounded
    } else {
        Bound::Included(start.parse().ok()?)
    };

    let end = if end == "" {
        Bound::Unbounded
    } else {
        Bound::Included(end.parse().ok()?)
    };

    Some(RangeCustom { start, end })
}
