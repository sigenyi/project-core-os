//! Version ordering.
//!
//! Versions are compared segment by segment, numeric runs numerically and alphabetic
//! runs lexically, so `1.10 > 1.9` and `2.4.0 > 2.4`. A `~` sorts before anything,
//! so `1.0~rc1 < 1.0`. The packaging release breaks ties.

use std::cmp::Ordering;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub upstream: String,
    pub release: u32,
}

impl Version {
    pub fn new(upstream: &str, release: u32) -> Self {
        Version { upstream: upstream.to_string(), release }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.upstream, self.release)
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        compare_upstream(&self.upstream, &other.upstream).then(self.release.cmp(&other.release))
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub fn compare_upstream(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.as_bytes(), b.as_bytes());
    loop {
        // Tilde first: it makes a version older than one without it.
        match (a.first() == Some(&b'~'), b.first() == Some(&b'~')) {
            (true, true) => {
                a = &a[1..];
                b = &b[1..];
                continue;
            }
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            _ => {}
        }
        // Skip separators.
        let skip = |s: &[u8]| s.iter().position(|c| c.is_ascii_alphanumeric() || *c == b'~').unwrap_or(s.len());
        a = &a[skip(a)..];
        b = &b[skip(b)..];
        if a.first() == Some(&b'~') || b.first() == Some(&b'~') {
            continue;
        }
        match (a.is_empty(), b.is_empty()) {
            (true, true) => return Ordering::Equal,
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            _ => {}
        }
        let numeric = a[0].is_ascii_digit();
        let take = |s: &[u8]| {
            s.iter()
                .position(|c| if numeric { !c.is_ascii_digit() } else { !c.is_ascii_alphabetic() })
                .unwrap_or(s.len())
        };
        if b[0].is_ascii_digit() != numeric {
            // Numbers sort after letters (1.0a < 1.0.1).
            return if numeric { Ordering::Greater } else { Ordering::Less };
        }
        let (sa, sb) = (&a[..take(a)], &b[..take(b)]);
        let ord = if numeric {
            let ta = trim_zeros(sa);
            let tb = trim_zeros(sb);
            ta.len().cmp(&tb.len()).then(ta.cmp(tb))
        } else {
            sa.cmp(sb)
        };
        if ord != Ordering::Equal {
            return ord;
        }
        a = &a[sa.len()..];
        b = &b[sb.len()..];
    }
}

fn trim_zeros(s: &[u8]) -> &[u8] {
    let start = s.iter().position(|c| *c != b'0').unwrap_or(s.len());
    &s[start..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering() {
        let lt = |a: &str, b: &str| assert_eq!(compare_upstream(a, b), Ordering::Less, "{a} < {b}");
        lt("1.9", "1.10");
        lt("2.4", "2.4.0");
        lt("1.0~rc1", "1.0");
        lt("1.0a", "1.0.1");
        lt("9.6", "9.7");
        lt("6.6+20251231", "6.6+20260101");
        lt("259.5", "260");
        assert_eq!(compare_upstream("1.01", "1.1"), Ordering::Equal);
        assert!(Version::new("1.0", 1) < Version::new("1.0", 2));
        assert!(Version::new("1.1", 1) > Version::new("1.0", 9));
    }
}
