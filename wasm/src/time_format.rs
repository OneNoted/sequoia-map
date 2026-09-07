use std::fmt::Write;

/// Format held seconds into fixed-width HH:MM:SS with cumulative hours.
pub fn format_hms(total_secs: i64) -> String {
    let mut out = String::with_capacity(8);
    write_hms(&mut out, total_secs);
    out
}

pub fn write_hms(buf: &mut String, total_secs: i64) {
    buf.clear();
    let secs = total_secs.max(0);
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;
    let _ = write!(buf, "{hours:02}:{minutes:02}:{seconds:02}");
}

#[cfg(test)]
mod tests {
    use super::format_hms;

    #[test]
    fn formats_zero() {
        assert_eq!(format_hms(0), "00:00:00");
    }

    #[test]
    fn formats_seconds_only() {
        assert_eq!(format_hms(59), "00:00:59");
    }

    #[test]
    fn formats_exact_minute() {
        assert_eq!(format_hms(60), "00:01:00");
    }

    #[test]
    fn formats_hour_minute_second() {
        assert_eq!(format_hms(3661), "01:01:01");
    }

    #[test]
    fn formats_cumulative_hours() {
        assert_eq!(format_hms(90061), "25:01:01");
    }

    #[test]
    fn clamps_negative() {
        assert_eq!(format_hms(-5), "00:00:00");
    }
}
