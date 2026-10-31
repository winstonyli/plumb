//! Text formatting for cells. Pure.

use std::fmt::{self, Display, Formatter};

use plumb_core::{NodeId, Tree};

/// A value shown as text, formatted straight into whatever it is written to: a `format!` argument, a `write!` into a
/// reused buffer, or a painter's scratch. Build one with the functions below.
#[derive(Clone, Copy)]
pub enum Show<'a> {
    Bytes(u64),
    /// A rate in bytes per second.
    Rate(u64),
    Count(u64),
    /// A share held in tenths of a percent: 255 is "25.5%".
    Tenths(u64),
    Date(u32),
    Age { now: u32, day: u32 },
    Noun { n: u64, one: &'a str, many: &'a str },
    /// A node's name, or its path (a drive's root with its trailing backslash).
    Name(&'a Tree, NodeId),
    Path(&'a Tree, NodeId),
}

/// "1.5 GB" style, 1024-based, one decimal above bytes.
pub fn size(b: u64) -> Show<'static> {
    Show::Bytes(b)
}

/// "1.5 GB/s": `size` per second.
pub fn rate(b: u64) -> Show<'static> {
    Show::Rate(b)
}

/// 1234567 -> "1,234,567".
pub fn count(n: u64) -> Show<'static> {
    Show::Count(n)
}

/// A share in tenths of a percent, as "25.5%".
pub fn tenths(t: u64) -> Show<'static> {
    Show::Tenths(t)
}

/// `YYYY-MM-DD` for a day count since 1601-01-01.
pub fn date(day: u32) -> Show<'static> {
    Show::Date(day)
}

/// How long before `now` a day was, as a phrase: "today", "5 d ago", "3 mo ago", "2.1 y ago".
pub fn age(now: u32, day: u32) -> Show<'static> {
    Show::Age { now, day }
}

/// A count with its noun: `noun(1, "item", "items")` is "1 item", `noun(1234, ..)` is "1,234 items".
pub fn noun<'a>(n: u64, one: &'a str, many: &'a str) -> Show<'a> {
    Show::Noun { n, one, many }
}

/// The name of a node.
pub fn node_name(tree: &Tree, id: NodeId) -> Show<'_> {
    Show::Name(tree, id)
}

/// A tree path as the shell and the user see it: "C:" is the drive's root node, but they need "C:\".
pub fn node_path(tree: &Tree, id: NodeId) -> Show<'_> {
    Show::Path(tree, id)
}

impl Display for Show<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match *self {
            Show::Bytes(b) => write_size(f, b),
            Show::Rate(b) => {
                write_size(f, b)?;
                f.write_str("/s")
            }
            Show::Count(n) => write_count(f, n),
            Show::Tenths(t) => write!(f, "{:.1}%", t as f64 / 10.0),
            Show::Date(day) => write_date(f, day),
            Show::Age { now, day } => {
                let d = now.saturating_sub(day);
                match d {
                    0 => f.write_str("today"),
                    1..=59 => write!(f, "{d} d ago"),
                    60..=729 => write!(f, "{} mo ago", (d + 15) / 30),
                    _ => write!(f, "{:.1} y ago", d as f32 / 365.0),
                }
            }
            Show::Noun { n, one, many } => {
                write_count(f, n)?;
                f.write_str(" ")?;
                f.write_str(if n == 1 { one } else { many })
            }
            Show::Name(tree, id) => tree.write_name(id, f),
            Show::Path(tree, id) => {
                tree.write_path(id, f)?;
                if tree.name(id).last() == Some(&u16::from(b':')) {
                    f.write_str("\\")?;
                }
                Ok(())
            }
        }
    }
}

/// Tests compare a shown value with another or with the text it should read as.
#[cfg(test)]
impl fmt::Debug for Show<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
    }
}

#[cfg(test)]
impl PartialEq for Show<'_> {
    fn eq(&self, o: &Self) -> bool {
        self.to_string() == o.to_string()
    }
}

#[cfg(test)]
impl PartialEq<&str> for Show<'_> {
    fn eq(&self, o: &&str) -> bool {
        /// Eats what is written from the front of the text it is compared with.
        struct Eat<'a>(&'a str);
        impl fmt::Write for Eat<'_> {
            fn write_str(&mut self, s: &str) -> fmt::Result {
                self.0 = self.0.strip_prefix(s).ok_or(fmt::Error)?;
                Ok(())
            }
        }
        let mut eat = Eat(o);
        fmt::write(&mut eat, format_args!("{self}")).is_ok() && eat.0.is_empty()
    }
}

fn write_size(f: &mut Formatter<'_>, b: u64) -> fmt::Result {
    const U: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if b < 1024 {
        return write!(f, "{b} B");
    }
    let mut v = b as f64;
    let mut u = 0;
    while v >= 1024.0 && u < U.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    write!(f, "{v:.1} {}", U[u])
}

fn write_count(f: &mut Formatter<'_>, mut n: u64) -> fmt::Result {
    // 20 digits and 6 separators at most, written from the right.
    let mut buf = [0u8; 26];
    let (mut i, mut digits) = (buf.len(), 0);
    loop {
        if digits > 0 && digits % 3 == 0 {
            i -= 1;
            buf[i] = b',';
        }
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        digits += 1;
        if n == 0 {
            break;
        }
    }
    f.write_str(std::str::from_utf8(&buf[i..]).unwrap_or_default())
}

fn write_date(f: &mut Formatter<'_>, day: u32) -> fmt::Result {
    let z = i64::from(day) - 134_774 + 719_468; // days since 0000-03-01
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    write!(f, "{y:04}-{m:02}-{d:02}")
}

/// Share of `part` in `whole` as 0.0..=1.0; 0 when `whole` is 0.
pub fn fraction(part: u64, whole: u64) -> f32 {
    if whole == 0 { 0.0 } else { (part as f64 / whole as f64) as f32 }
}

/// The local calendar day (since 1601) of a FILETIME (100 ns since 1601, UTC), by the machine's time zone as set now.
pub fn local_day(filetime: u64) -> u32 {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::Storage::FileSystem::FileTimeToLocalFileTime;
    let utc = FILETIME { dwLowDateTime: filetime as u32, dwHighDateTime: (filetime >> 32) as u32 };
    let mut local = FILETIME::default();
    let ft = if unsafe { FileTimeToLocalFileTime(&utc, &mut local) }.is_ok() { (u64::from(local.dwHighDateTime) << 32) | u64::from(local.dwLowDateTime) } else { filetime };
    (ft / 864_000_000_000) as u32
}

/// The first `max` paths, one a line, then "and N more".
pub fn path_list(paths: &[String], max: usize) -> String {
    let mut out = paths.iter().take(max).map(String::as_str).collect::<Vec<_>>().join("\n");
    if paths.len() > max {
        out.push_str(&format!("\nand {} more", paths.len() - max));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_list_shows_the_first_few_and_counts_the_rest() {
        let p: Vec<String> = ["C:\\a", "C:\\b", "C:\\c"].map(String::from).into();
        assert_eq!(path_list(&p, 5), "C:\\a\nC:\\b\nC:\\c");
        assert_eq!(path_list(&p, 2), "C:\\a\nC:\\b\nand 1 more");
        assert_eq!(path_list(&[], 2), "");
    }

    #[test]
    fn the_local_day_is_the_utc_day_moved_by_the_zone_and_the_same_for_two_instants_in_one_local_day() {
        let day = 864_000_000_000u64;
        let t = 134_774 * day + 12 * 36_000_000_000; // noon UTC on 1970-01-01
        let d = i64::from(local_day(t)) - 134_774;
        assert!((-1..=1).contains(&d), "within a day of UTC, got {d}");
        // two instants an hour apart land on the same local day unless a local midnight lies between them
        let (a, b) = (local_day(t), local_day(t + 36_000_000_000));
        assert!(a == b || a + 1 == b);
        // one UTC day later is exactly one local day later (no DST step in 1970-01-01..02 for any zone rule applied today)
        assert_eq!(local_day(t + day), local_day(t) + 1);
    }

    #[test]
    fn dates_and_ages() {
        assert_eq!(date(134_774).to_string(), "1970-01-01");
        assert_eq!(date(134_774 + 19_791).to_string(), "2024-03-09");
        assert_eq!(date(134_774 + 19_782).to_string(), "2024-02-29"); // a leap day
        assert_eq!(age(100, 100).to_string(), "today");
        assert_eq!(age(105, 100).to_string(), "5 d ago");
        assert_eq!(age(100 + 90, 100).to_string(), "3 mo ago");
        assert_eq!(age(100 + 766, 100).to_string(), "2.1 y ago");
        assert_eq!(age(100, 200).to_string(), "today", "a time in the future is today");
    }

    #[test]
    fn a_drive_root_gets_a_trailing_backslash_and_a_name_is_just_its_name() {
        use plumb_core::{NodeFlags, TreeBuilder};
        let mut b = TreeBuilder::new();
        b.push_str(NodeId::NONE, "C:", 0, NodeFlags::DIR);
        b.push_str(NodeId(0), "Windows", 0, NodeFlags::DIR);
        let t = b.finish().unwrap();
        assert_eq!(node_path(&t, NodeId(0)).to_string(), "C:\\");
        assert_eq!(node_path(&t, NodeId(1)).to_string(), "C:\\Windows");
        assert_eq!(node_name(&t, NodeId(1)).to_string(), "Windows");
    }

    #[test]
    fn sizes_rates_and_shares() {
        assert_eq!(size(0).to_string(), "0 B");
        assert_eq!(size(1023).to_string(), "1023 B");
        assert_eq!(size(1024).to_string(), "1.0 KB");
        assert_eq!(size(1536).to_string(), "1.5 KB");
        assert_eq!(size(5 * 1024 * 1024 * 1024).to_string(), "5.0 GB");
        assert_eq!(rate(2048).to_string(), "2.0 KB/s");
        assert_eq!(tenths(255).to_string(), "25.5%");
        assert_eq!(tenths(0).to_string(), "0.0%");
    }

    #[test]
    fn a_count_agrees_with_its_noun() {
        assert_eq!(noun(0, "item", "items").to_string(), "0 items");
        assert_eq!(noun(1, "item", "items").to_string(), "1 item");
        assert_eq!(noun(2, "smaller process", "smaller processes").to_string(), "2 smaller processes");
        assert_eq!(noun(1234, "file", "files").to_string(), "1,234 files");
    }

    #[test]
    fn counts_and_fractions() {
        assert_eq!(count(0).to_string(), "0");
        assert_eq!(count(999).to_string(), "999");
        assert_eq!(count(1000).to_string(), "1,000");
        assert_eq!(count(1234567).to_string(), "1,234,567");
        assert_eq!(count(u64::MAX).to_string(), "18,446,744,073,709,551,615");
        assert_eq!(fraction(1, 4), 0.25);
        assert_eq!(fraction(5, 0), 0.0);
    }
}
