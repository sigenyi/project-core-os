//! Package metadata: the manifest (`.CORE/manifest.toml`) and the file list
//! (`.CORE/files`).
//!
//! Besides the usual name, version and dependencies, a manifest records what a
//! package *provides* in terms the AI can use directly: programs, shared libraries,
//! systemd units, configuration files and manual pages, plus an optional `[ai]`
//! section describing how the software is meant to be used.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::version::Version;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub package: PackageInfo,
    #[serde(default)]
    pub depends: Depends,
    #[serde(default)]
    pub provides: Provides,
    #[serde(default, skip_serializing_if = "AiInfo::is_empty")]
    pub ai: AiInfo,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageInfo {
    pub name: String,
    pub version: String,
    /// Packaging revision of the same upstream version.
    pub release: u32,
    pub arch: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub license: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub homepage: String,
    /// Total size of the payload in bytes.
    #[serde(default)]
    pub installed_size: u64,
    /// Unix time the package was built (SOURCE_DATE_EPOCH when set).
    #[serde(default)]
    pub build_date: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Depends {
    /// Packages (or provided names) needed at run time, declared by the recipe.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub packages: Vec<String>,
    /// Shared libraries needed at run time, detected from ELF `DT_NEEDED`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub libraries: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provides {
    /// Extra names this package answers to (e.g. "sh" for bash).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
    /// Executables in `usr/bin`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub binaries: Vec<String>,
    /// Shared library sonames.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub libraries: Vec<String>,
    /// systemd units shipped in `usr/lib/systemd/{system,user}`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<String>,
    /// Configuration files (everything under `etc/`), preserved across upgrades.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub config: Vec<String>,
    /// Manual pages, as `name.section`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub man: Vec<String>,
}

/// How the AI should think about this software.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AiInfo {
    /// One of: cli, tui, gui, service, library, data, system.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    /// Program to launch when the user asks for this software by name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub launch: String,
    /// Words users might use for it ("text editor", "web browser").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    /// Common operations, with the command that performs each.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<Operation>,
}

impl AiInfo {
    pub fn is_empty(&self) -> bool {
        self.kind.is_empty() && self.launch.is_empty() && self.keywords.is_empty() && self.operations.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub name: String,
    pub description: String,
    /// argv template; `{param}` placeholders are filled from validated arguments.
    pub command: Vec<String>,
    /// Whether it changes system state (and therefore needs confirmation).
    #[serde(default)]
    pub mutating: bool,
}

impl Manifest {
    pub fn version(&self) -> Version {
        Version::new(&self.package.version, self.package.release)
    }

    /// `name-version-release`
    pub fn id(&self) -> String {
        format!("{}-{}-{}", self.package.name, self.package.version, self.package.release)
    }

    pub fn file_name(&self) -> String {
        format!("{}-{}-{}.{}.cpk", self.package.name, self.package.version, self.package.release, self.package.arch)
    }

    pub fn from_toml(text: &str) -> Result<Self, String> {
        let m: Manifest = toml::from_str(text).map_err(|e| format!("invalid manifest: {e}"))?;
        m.validate()?;
        Ok(m)
    }

    pub fn to_toml(&self) -> String {
        toml::to_string(self).expect("manifest serialises")
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_name(&self.package.name)?;
        if self.package.version.is_empty()
            || !self.package.version.chars().all(|c| c.is_ascii_alphanumeric() || "._+~".contains(c))
        {
            return Err(format!("invalid version {:?}", self.package.version));
        }
        for d in &self.depends.packages {
            validate_name(d)?;
        }
        Ok(())
    }
}

/// Package names: lowercase letters, digits and `-._+`, starting alphanumerically.
pub fn validate_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-._+".contains(c));
    if ok { Ok(()) } else { Err(format!("invalid package name {name:?}")) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    File,
    Dir,
    Symlink,
    /// A hard link to another file of the same package.
    Hardlink,
}

impl FileKind {
    fn as_str(self) -> &'static str {
        match self {
            FileKind::File => "f",
            FileKind::Dir => "d",
            FileKind::Symlink => "l",
            FileKind::Hardlink => "h",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "f" => FileKind::File,
            "d" => FileKind::Dir,
            "l" => FileKind::Symlink,
            "h" => FileKind::Hardlink,
            _ => return None,
        })
    }
}

/// One entry of a package's payload. Paths are relative to the root, without a
/// leading slash, and never contain `..`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub kind: FileKind,
    pub mode: u32,
    pub size: u64,
    /// SHA-256 of the content (regular files only).
    pub sha256: String,
    /// Symlink or hardlink target.
    pub target: String,
}

impl FileEntry {
    pub fn is_config(&self) -> bool {
        self.kind == FileKind::File && self.path.starts_with("etc/")
    }
}

impl fmt::Display for FileEntry {
    /// Tab-separated: kind, octal mode, size, sha256 (or -), path, target (or -).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let dash = |s: &str| if s.is_empty() { "-".to_string() } else { s.to_string() };
        write!(
            f,
            "{}\t{:o}\t{}\t{}\t{}\t{}",
            self.kind.as_str(),
            self.mode,
            self.size,
            dash(&self.sha256),
            self.path,
            dash(&self.target)
        )
    }
}

/// Reject absolute paths, `..`, empty components and control characters.
pub fn check_relative_path(path: &str) -> Result<(), String> {
    if path.is_empty() || path.starts_with('/') || path.ends_with('/') {
        return Err(format!("invalid payload path {path:?}"));
    }
    if path.split('/').any(|c| c.is_empty() || c == "." || c == "..") || path.chars().any(|c| c.is_control()) {
        return Err(format!("invalid payload path {path:?}"));
    }
    Ok(())
}

pub fn parse_file_list(text: &str) -> Result<Vec<FileEntry>, String> {
    text.lines()
        .filter(|l| !l.is_empty())
        .map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() != 6 {
                return Err(format!("malformed file list line: {line:?}"));
            }
            let undash = |s: &str| if s == "-" { String::new() } else { s.to_string() };
            let entry = FileEntry {
                kind: FileKind::parse(f[0]).ok_or_else(|| format!("bad file kind in {line:?}"))?,
                mode: u32::from_str_radix(f[1], 8).map_err(|_| format!("bad mode in {line:?}"))?,
                size: f[2].parse().map_err(|_| format!("bad size in {line:?}"))?,
                sha256: undash(f[3]),
                path: f[4].to_string(),
                target: undash(f[5]),
            };
            check_relative_path(&entry.path)?;
            Ok(entry)
        })
        .collect()
}

pub fn format_file_list(files: &[FileEntry]) -> String {
    let mut out = String::new();
    for f in files {
        out.push_str(&f.to_string());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Manifest {
        Manifest::from_toml(
            r#"
            [package]
            name = "nano"
            version = "8.7.1"
            release = 1
            arch = "x86_64"
            summary = "Small text editor"
            [depends]
            packages = ["ncurses"]
            libraries = ["libncursesw.so.6", "libc.so.6"]
            [provides]
            binaries = ["nano", "rnano"]
            config = ["etc/nanorc"]
            [ai]
            kind = "tui"
            launch = "nano"
            keywords = ["text editor", "editor"]
            [[ai.operations]]
            name = "edit"
            description = "Open a file for editing"
            command = ["nano", "{file}"]
            "#,
        )
        .unwrap()
    }

    #[test]
    fn manifest_round_trip() {
        let m = sample();
        assert_eq!(m.id(), "nano-8.7.1-1");
        assert_eq!(m.file_name(), "nano-8.7.1-1.x86_64.cpk");
        assert_eq!(Manifest::from_toml(&m.to_toml()).unwrap(), m);
        assert_eq!(m.ai.operations[0].command, ["nano", "{file}"]);
    }

    #[test]
    fn names_and_versions_are_checked() {
        assert!(validate_name("libstdc++").is_ok());
        assert!(validate_name("Bad").is_err());
        assert!(validate_name("-x").is_err());
        let mut m = sample();
        m.package.version = "1.0;rm".into();
        assert!(m.validate().is_err());
    }

    #[test]
    fn file_list_round_trip_and_safety() {
        let files = vec![
            FileEntry {
                path: "usr/bin/nano".into(),
                kind: FileKind::File,
                mode: 0o755,
                size: 10,
                sha256: "ab".into(),
                target: String::new(),
            },
            FileEntry {
                path: "usr/bin/rnano".into(),
                kind: FileKind::Symlink,
                mode: 0o777,
                size: 0,
                sha256: String::new(),
                target: "nano".into(),
            },
            FileEntry {
                path: "etc/nanorc".into(),
                kind: FileKind::File,
                mode: 0o644,
                size: 1,
                sha256: "cd".into(),
                target: String::new(),
            },
        ];
        let text = format_file_list(&files);
        assert_eq!(parse_file_list(&text).unwrap(), files);
        assert!(files[2].is_config() && !files[0].is_config());
        for bad in ["/etc/x", "a/../b", "a//b", "a/", "", "./a"] {
            assert!(check_relative_path(bad).is_err(), "{bad}");
        }
        assert!(parse_file_list("f\t644\t1\tab\t../../etc/shadow\t-\n").is_err());
    }
}
