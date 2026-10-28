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
    fn formats_cumulative_hours() {
        assert_eq!(format_hms(90061), "25:01:01");
    }

    #[test]
    fn clamps_negative() {
        assert_eq!(format_hms(-5), "00:00:00");
    }
}
