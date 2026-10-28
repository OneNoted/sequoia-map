//! Helpers the server's unit tests share.

use chrono::{DateTime, Utc};

/// An RFC 3339 test timestamp.
pub(crate) fn ts(value: &str) -> DateTime<Utc> {
    value.parse::<DateTime<Utc>>().expect("parse timestamp")
}
