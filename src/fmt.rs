//! Number, size and date formatting, and parsing of the date and size fields.

use chrono::{Local, NaiveDate, TimeZone};

pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{bytes} B") } else { format!("{v:.2} {}", UNITS[u]) }
}

/// Size in whole KB with thousands separators, the way Explorer shows it.
pub fn kb(bytes: u64) -> String {
    format!("{} KB", count(bytes.div_ceil(1024)))
}

pub fn count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Local date and time of unix seconds.
pub fn date(secs: i64) -> String {
    match Local.timestamp_opt(secs, 0).single() {
        Some(t) => t.format("%Y-%m-%d %H:%M").to_string(),
        None => String::new(),
    }
}

pub fn now_string() -> String {
    Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// A date field: empty, `YYYY-MM-DD`, or a relative age such as `7d`, `3w`, `6m`, `1y`, `12h`.
/// Returns local midnight of the date (or now minus the age) as unix seconds.
pub fn parse_date(s: &str) -> Result<Option<i64>, String> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    if s.eq_ignore_ascii_case("today") {
        return parse_date("0d");
    }
    if let Some(unit) = s.chars().last().filter(|c| c.is_ascii_alphabetic()) {
        let n: i64 = s[..s.len() - 1].trim().parse().map_err(|_| format!("'{s}' is not a date"))?;
        let now = Local::now();
        let secs = match unit.to_ascii_lowercase() {
            'h' => return Ok(Some(now.timestamp() - n * 3600)),
            'd' => n * 86_400,
            'w' => n * 7 * 86_400,
            'm' => n * 30 * 86_400,
            'y' => n * 365 * 86_400,
            _ => return Err(format!("'{s}': use h, d, w, m or y")),
        };
        // Whole days count back from the start of today.
        let midnight = Local.from_local_datetime(&now.date_naive().and_hms_opt(0, 0, 0).unwrap_or_default()).earliest();
        return Ok(Some(midnight.map_or(now.timestamp(), |m| m.timestamp()) - secs));
    }
    let d = NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| format!("'{s}' is not a date (YYYY-MM-DD or 7d)"))?;
    let t = Local.from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap_or_default()).earliest().ok_or_else(|| format!("'{s}' is not a valid local date"))?;
    Ok(Some(t.timestamp()))
}

/// A size field in `unit_bytes` units: empty, or a number (decimals allowed).
pub fn parse_size(s: &str, unit_bytes: u64) -> Result<Option<u64>, String> {
    let s = s.trim().replace(',', "");
    if s.is_empty() {
        return Ok(None);
    }
    let v: f64 = s.parse().map_err(|_| format!("'{s}' is not a size"))?;
    if v < 0.0 {
        return Err("sizes cannot be negative".into());
    }
    Ok(Some((v * unit_bytes as f64) as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(count(5745143), "5,745,143");
        assert_eq!(count(12), "12");
        assert_eq!(kb(1), "1 KB");
        assert_eq!(human(512), "512 B");
    }

    #[test]
    fn parses() {
        assert_eq!(parse_date("").unwrap(), None);
        assert!(parse_date("2026-08-23").unwrap().is_some());
        assert!(parse_date("7d").unwrap().unwrap() < parse_date("1d").unwrap().unwrap());
        assert!(parse_date("8/23/2026").is_err());
        assert_eq!(parse_size("1.5", 1024).unwrap(), Some(1536));
        assert!(parse_size("x", 1).is_err());
    }
}
