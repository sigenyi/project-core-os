//! Filesystem access relative to a root directory.
//!
//! Every collector reads `/proc`, `/sys`, `/etc` and `/dev/kmsg` through a [`Sysroot`].
//! In production the root is `/`; in tests it is a fixture tree, which lets the whole
//! perception pipeline be exercised without real hardware.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Sysroot {
    root: PathBuf,
}

impl Default for Sysroot {
    fn default() -> Self {
        Sysroot::live()
    }
}

impl Sysroot {
    /// The running system.
    pub fn live() -> Self {
        Sysroot { root: PathBuf::from("/") }
    }

    /// A fixture tree rooted at `root`.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Sysroot { root: root.into() }
    }

    /// True when reading the running system (enables syscalls and subprocess probes
    /// that cannot be redirected to a fixture tree).
    pub fn is_live(&self) -> bool {
        self.root == Path::new("/")
    }

    /// Resolve an absolute system path (e.g. `/proc/meminfo`) inside the root.
    pub fn path(&self, abs: impl AsRef<Path>) -> PathBuf {
        let abs = abs.as_ref();
        self.root.join(abs.strip_prefix("/").unwrap_or(abs))
    }

    pub fn read(&self, abs: impl AsRef<Path>) -> Option<String> {
        fs::read_to_string(self.path(abs)).ok()
    }

    /// Read and trim; empty files yield `None`.
    pub fn read_trim(&self, abs: impl AsRef<Path>) -> Option<String> {
        self.read(abs).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }

    pub fn read_u64(&self, abs: impl AsRef<Path>) -> Option<u64> {
        self.read_trim(abs)?.parse().ok()
    }

    pub fn read_i64(&self, abs: impl AsRef<Path>) -> Option<i64> {
        self.read_trim(abs)?.parse().ok()
    }

    pub fn exists(&self, abs: impl AsRef<Path>) -> bool {
        self.path(abs).exists()
    }

    /// Sorted entry names of a directory (empty if it does not exist).
    pub fn list(&self, abs: impl AsRef<Path>) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.path(abs))
            .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        names.sort_by(|a, b| natural_cmp(a, b));
        names
    }

    /// Final path component of a symlink's target (`.../driver -> ../../iwlwifi` gives `iwlwifi`).
    pub fn link_name(&self, abs: impl AsRef<Path>) -> Option<String> {
        let target = fs::read_link(self.path(abs)).ok()?;
        target.file_name().map(|n| n.to_string_lossy().into_owned())
    }

    /// Full target of a symlink, as stored.
    pub fn link_target(&self, abs: impl AsRef<Path>) -> Option<String> {
        fs::read_link(self.path(abs)).ok().map(|t| t.to_string_lossy().into_owned())
    }
}

/// Compare strings so that embedded numbers sort numerically (`card2 < card10`).
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let split = |s: &str| -> (String, Option<u64>) {
        let digits: String =
            s.chars().rev().take_while(|c| c.is_ascii_digit()).collect::<Vec<_>>().into_iter().rev().collect();
        if digits.is_empty() || digits.len() > 18 {
            (s.to_string(), None)
        } else {
            (s[..s.len() - digits.len()].to_string(), digits.parse().ok())
        }
    };
    let (pa, na) = split(a);
    let (pb, nb) = split(b);
    pa.cmp(&pb).then(na.cmp(&nb)).then(a.cmp(b))
}

/// Parse `key: value` / `KEY=value` style files into pairs.
pub fn key_values<'a>(text: &'a str, sep: char) -> impl Iterator<Item = (&'a str, &'a str)> + 'a {
    text.lines().filter_map(move |line| {
        let (k, v) = line.split_once(sep)?;
        Some((k.trim(), v.trim().trim_matches('"')))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_ordering() {
        let mut v = vec!["card10", "card2", "card1", "eth0", "dev"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["card1", "card2", "card10", "dev", "eth0"]);
    }

    #[test]
    fn path_mapping() {
        let r = Sysroot::at("/tmp/fixture");
        assert_eq!(r.path("/proc/meminfo"), PathBuf::from("/tmp/fixture/proc/meminfo"));
        assert!(!r.is_live());
        assert!(Sysroot::live().is_live());
    }
}
