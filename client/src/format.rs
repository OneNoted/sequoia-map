//! How the map writes amounts and times, so every surface says the same number the same way.

/// A count or amount, exact below a thousand: `300`, `9.0k`, `2.9M`.
pub(crate) fn compact(value: i64) -> String {
    let magnitude = value.unsigned_abs();
    let sign = if value < 0 { "-" } else { "" };
    if magnitude >= 1_000_000 {
        format!("{sign}{:.1}M", magnitude as f64 / 1_000_000.0)
    } else if magnitude >= 1_000 {
        format!("{sign}{:.1}k", magnitude as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

/// A rate with as many decimals as its size warrants: `28.7k`, `340`, `39.9`, `2.50`.
pub(crate) fn rate(value: f64) -> String {
    if value >= 1000.0 {
        format!("{:.1}k", value / 1000.0)
    } else if value >= 100.0 {
        format!("{value:.0}")
    } else if value >= 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.2}")
    }
}

/// How long ago, coarsely: `just now`, `6m ago`, `2h ago`, `3d ago`, `2w ago`.
pub(crate) fn ago(seconds: i64) -> String {
    match seconds.max(0) {
        0..60 => "just now".to_string(),
        seconds @ 60..3_600 => format!("{}m ago", seconds / 60),
        seconds @ 3_600..86_400 => format!("{}h ago", seconds / 3_600),
        seconds @ 86_400..604_800 => format!("{}d ago", seconds / 86_400),
        seconds => format!("{}w ago", seconds / 604_800),
    }
}

/// An RFC 3339 time relative to `now`; past a month, its date. Unparseable input is shown as is.
pub(crate) fn since(rfc3339: &str, now: i64) -> String {
    let Ok(time) = chrono::DateTime::parse_from_rfc3339(rfc3339) else {
        return rfc3339.to_string();
    };
    let seconds = now - time.timestamp();
    if seconds >= 30 * 86_400 {
        time.format("%b %d, %Y").to_string()
    } else {
        ago(seconds)
    }
}

/// A Unix time as the viewer's local wall-clock time, `14:02`.
pub(crate) fn local_time(at: i64) -> String {
    local(at).map_or_else(String::new, |time| time.format("%H:%M").to_string())
}

/// The same with seconds, for times a few seconds apart.
pub(crate) fn local_time_seconds(at: i64) -> String {
    local(at).map_or_else(String::new, |time| time.format("%H:%M:%S").to_string())
}

fn local(at: i64) -> Option<chrono::DateTime<chrono::Local>> {
    chrono::DateTime::from_timestamp(at, 0).map(|time| time.with_timezone(&chrono::Local))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_are_compact_and_exact_while_small() {
        assert_eq!(compact(300), "300");
        assert_eq!(compact(9_000), "9.0k");
        assert_eq!(compact(16_787), "16.8k");
        assert_eq!(compact(2_900_000), "2.9M");
        assert_eq!(compact(-1_500), "-1.5k");
    }

    #[test]
    fn rates_keep_the_decimals_their_size_needs() {
        assert_eq!(rate(28_700.0), "28.7k");
        assert_eq!(rate(340.4), "340");
        assert_eq!(rate(39.94), "39.9");
        assert_eq!(rate(2.5), "2.50");
    }

    #[test]
    fn ago_is_coarse() {
        assert_eq!(ago(-5), "just now");
        assert_eq!(ago(59), "just now");
        assert_eq!(ago(61), "1m ago");
        assert_eq!(ago(7_300), "2h ago");
        assert_eq!(ago(3 * 86_400 + 5), "3d ago");
        assert_eq!(ago(15 * 86_400), "2w ago");
    }

    #[test]
    fn since_turns_to_a_date_after_a_month() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-27T12:00:00Z")
            .unwrap()
            .timestamp();
        assert_eq!(since("2026-10-27T11:00:00Z", now), "1h ago");
        assert_eq!(since("2022-10-14T00:00:00Z", now), "Oct 14, 2022");
        assert_eq!(since("not a time", now), "not a time");
    }
}
