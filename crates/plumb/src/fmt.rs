//! Text formatting for cells. Pure.

/// "1.5 GB" style, 1024-based, one decimal above bytes.
pub fn size(b: u64) -> String {
    const U: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if b < 1024 {
        return format!("{b} B");
    }
    let mut v = b as f64;
    let mut u = 0;
    while v >= 1024.0 && u < U.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    format!("{v:.1} {}", U[u])
}

/// 1234567 -> "1,234,567".
pub fn count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Share of `part` in `whole` as 0.0..=1.0; 0 when `whole` is 0.
pub fn fraction(part: u64, whole: u64) -> f32 {
    if whole == 0 { 0.0 } else { (part as f64 / whole as f64) as f32 }
}

/// A tree path as the shell and the user see it: "C:" is the drive's root node, but they need "C:\".
pub fn display_path(p: &str) -> String {
    if p.ends_with(':') { format!("{p}\\") } else { p.to_string() }
}

/// `YYYY-MM-DD` for a day count since 1601-01-01.
pub fn date(day: u32) -> String {
    let z = i64::from(day) - 134_774 + 719_468; // days since 0000-03-01
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// How long before `now` a day was: "today", "5 d", "3 mo", "2.1 y".
pub fn age(now: u32, day: u32) -> String {
    let d = now.saturating_sub(day);
    match d {
        0 => "today".into(),
        1..=59 => format!("{d} d"),
        60..=729 => format!("{} mo", (d + 15) / 30),
        _ => format!("{:.1} y", d as f32 / 365.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_ages() {
        assert_eq!(date(134_774), "1970-01-01");
        assert_eq!(date(134_774 + 19_791), "2024-03-09");
        assert_eq!(date(134_774 + 19_782), "2024-02-29"); // a leap day
        assert_eq!(age(100, 100), "today");
        assert_eq!(age(105, 100), "5 d");
        assert_eq!(age(100 + 90, 100), "3 mo");
        assert_eq!(age(100 + 766, 100), "2.1 y");
        assert_eq!(age(100, 200), "today", "a time in the future is today");
    }

    #[test]
    fn drive_root_gets_a_trailing_backslash() {
        assert_eq!(display_path("C:"), "C:\\");
        assert_eq!(display_path("C:\\Windows"), "C:\\Windows");
    }

    #[test]
    fn sizes() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(1023), "1023 B");
        assert_eq!(size(1024), "1.0 KB");
        assert_eq!(size(1536), "1.5 KB");
        assert_eq!(size(5 * 1024 * 1024 * 1024), "5.0 GB");
    }

    #[test]
    fn counts_and_fractions() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1000), "1,000");
        assert_eq!(count(1234567), "1,234,567");
        assert_eq!(fraction(1, 4), 0.25);
        assert_eq!(fraction(5, 0), 0.0);
    }
}
