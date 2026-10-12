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
        if i > 0 && (s.len() - i) % 3 == 0 {
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

#[cfg(test)]
mod tests {
    use super::*;

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
