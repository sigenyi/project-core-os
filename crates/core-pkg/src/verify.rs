//! Checking installed files against the package database.
//!
//! A file that is missing, changed or replaced by something else is an integrity
//! problem. Configuration is different: files under `/etc` are there to be edited
//! (and some, like the account databases, are changed by install hooks), so a
//! changed configuration file is reported but is not a failure.

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use serde::Serialize;

use crate::archive::sha256_file;
use crate::db::Db;
use crate::manifest::{FileEntry, FileKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Problem {
    /// Not there at all.
    Missing,
    /// Different content, or a symlink pointing somewhere else.
    Modified,
    /// There, but a different kind of file (a directory where a file was, a
    /// symlink where a regular file was, ...).
    Replaced,
    /// A configuration file (or symlink) under `/etc` that differs from the
    /// package and is still a regular file or a symlink. Expected on a
    /// configured system; not an integrity failure.
    ModifiedConfig,
}

impl Problem {
    pub fn is_failure(self) -> bool {
        self != Problem::ModifiedConfig
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Problem::Missing => "missing",
            Problem::Modified => "modified",
            Problem::Replaced => "replaced",
            Problem::ModifiedConfig => "modified configuration",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub package: String,
    pub path: String,
    pub problem: Problem,
    /// Whether this finding is an integrity failure (everything except modified
    /// configuration).
    pub failure: bool,
}

/// Configuration is what a package installs under `etc/`: files and symlinks
/// (an administrator re-points `/etc/localtime`).
fn is_config(f: &FileEntry) -> bool {
    matches!(f.kind, FileKind::File | FileKind::Symlink) && f.path.starts_with("etc/")
}

/// What a configuration path may legitimately become: an edited file, or a
/// symlink (`/etc/resolv.conf` pointing at systemd-resolved's), or the reverse.
/// A directory, FIFO, socket or device node in its place is not configuration.
fn is_plain(t: fs::FileType) -> bool {
    t.is_file() || t.is_symlink()
}

/// `sha256` maps the package's regular files to their recorded content hashes.
fn check(root: &Path, f: &FileEntry, sha256: &HashMap<&str, &str>) -> Option<Problem> {
    let path = root.join(&f.path);
    let Ok(meta) = fs::symlink_metadata(&path) else { return Some(Problem::Missing) };
    let t = meta.file_type();
    // Something else in the place of a configuration file or symlink: still
    // configuration if it is a file or symlink, an integrity failure otherwise.
    let replaced = || if is_config(f) && is_plain(t) { Problem::ModifiedConfig } else { Problem::Replaced };
    let content = |want: &str| match sha256_file(&path) {
        Ok(h) if h == want => None,
        Ok(_) => Some(if is_config(f) { Problem::ModifiedConfig } else { Problem::Modified }),
        Err(_) => Some(Problem::Missing),
    };
    match f.kind {
        FileKind::File if !t.is_file() => Some(replaced()),
        FileKind::File => content(&f.sha256),
        FileKind::Hardlink if !t.is_file() => Some(Problem::Replaced),
        FileKind::Hardlink => {
            // Still a link to the file it was installed with: that file's content
            // is checked under its own entry.
            let linked = fs::symlink_metadata(root.join(&f.target))
                .is_ok_and(|m| (m.dev(), m.ino()) == (meta.dev(), meta.ino()));
            if linked {
                return None;
            }
            // The link was broken (deleted and recreated, say): its content must
            // still be what the package installed.
            match sha256.get(f.target.as_str()) {
                Some(want) => content(want),
                None => Some(Problem::Modified),
            }
        }
        FileKind::Symlink if !t.is_symlink() => Some(replaced()),
        FileKind::Symlink => match fs::read_link(&path) {
            Ok(target) if target == Path::new(&f.target) => None,
            _ => Some(if is_config(f) { Problem::ModifiedConfig } else { Problem::Modified }),
        },
        FileKind::Dir if !t.is_dir() => Some(Problem::Replaced),
        FileKind::Dir => None,
    }
}

/// Check the files of `packages` (every installed package when empty). Naming a
/// package that is not installed is an error.
pub fn verify(db: &Db, packages: &[String]) -> Result<Vec<Finding>, String> {
    if let Some(unknown) = packages.iter().find(|p| db.get(p).is_none()) {
        return Err(format!("{unknown} is not installed"));
    }
    let mut findings = Vec::new();
    for (name, p) in &db.packages {
        if !packages.is_empty() && !packages.contains(name) {
            continue;
        }
        let sha256: HashMap<&str, &str> =
            p.files.iter().filter(|f| f.kind == FileKind::File).map(|f| (f.path.as_str(), f.sha256.as_str())).collect();
        for f in &p.files {
            if let Some(problem) = check(db.root(), f, &sha256) {
                findings.push(Finding {
                    package: name.clone(),
                    path: f.path.clone(),
                    problem,
                    failure: problem.is_failure(),
                });
            }
        }
    }
    Ok(findings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::create_package;
    use crate::archive::tests::{manifest, stage};
    use crate::transaction::{Options, Report, install_package};
    use std::os::unix::fs::symlink;

    fn installed() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("stage");
        stage(&dest);
        symlink("../usr/share/zoneinfo/UTC", dest.join("etc/localtime")).unwrap();
        let pkg = create_package(&dest, manifest("hello", "1.0"), &dir.path().join("pkgs")).unwrap();
        let root = dir.path().join("root");
        fs::create_dir_all(&root).unwrap();
        let mut db = Db::open(&root).unwrap();
        install_package(&mut db, &pkg, &Options::default(), &mut Report::default(), &mut Vec::new()).unwrap();
        (dir, root)
    }

    fn problems(root: &Path) -> Vec<(String, Problem, bool)> {
        let db = Db::open(root).unwrap();
        verify(&db, &[]).unwrap().into_iter().map(|f| (f.path, f.problem, f.failure)).collect()
    }

    #[test]
    fn intact_installation_has_no_findings() {
        let (_dir, root) = installed();
        assert_eq!(problems(&root), vec![]);
    }

    #[test]
    fn edited_configuration_is_reported_but_not_a_failure() {
        let (_dir, root) = installed();
        fs::write(root.join("etc/hello.conf"), "greeting=mine\n").unwrap();
        fs::remove_file(root.join("etc/localtime")).unwrap();
        symlink("../usr/share/zoneinfo/Europe/Oslo", root.join("etc/localtime")).unwrap();
        assert_eq!(
            problems(&root),
            vec![
                ("etc/hello.conf".into(), Problem::ModifiedConfig, false),
                ("etc/localtime".into(), Problem::ModifiedConfig, false),
            ]
        );
    }

    #[test]
    fn integrity_problems_are_failures() {
        let (_dir, root) = installed();
        // Changed program, deleted manual page, retargeted program symlink, a
        // directory replaced by a file, and a deleted configuration file.
        fs::write(root.join("usr/bin/hello"), "#!/bin/sh\necho changed\n").unwrap();
        fs::remove_file(root.join("usr/share/man/man1/hello.1")).unwrap();
        fs::remove_file(root.join("usr/bin/hi")).unwrap();
        symlink("/tmp/evil", root.join("usr/bin/hi")).unwrap();
        fs::remove_dir_all(root.join("usr/lib/systemd/system")).unwrap();
        fs::write(root.join("usr/lib/systemd/system"), "").unwrap();
        fs::remove_file(root.join("etc/hello.conf")).unwrap();
        let found = problems(&root);
        let expect = [
            ("etc/hello.conf", Problem::Missing),
            ("usr/bin/hello", Problem::Modified),
            ("usr/bin/hi", Problem::Modified),
            ("usr/lib/systemd/system", Problem::Replaced),
            ("usr/lib/systemd/system/hello.service", Problem::Missing),
            ("usr/share/man/man1/hello.1", Problem::Missing),
        ];
        for (path, problem) in expect {
            assert!(found.contains(&(path.to_string(), problem, true)), "{path}: {problem:?} not in {found:?}");
        }
        // The hardlink to the changed program is not reported twice.
        assert!(!found.iter().any(|(p, ..)| p == "usr/bin/hello2"), "{found:?}");
    }

    #[test]
    fn a_regular_file_replaced_by_a_symlink_to_identical_content_is_caught() {
        let (_dir, root) = installed();
        let copy = root.join("usr/share/hello-copy");
        fs::copy(root.join("usr/bin/hello"), &copy).unwrap();
        fs::remove_file(root.join("usr/bin/hello")).unwrap();
        symlink(&copy, root.join("usr/bin/hello")).unwrap();
        assert!(problems(&root).contains(&("usr/bin/hello".into(), Problem::Replaced, true)));
    }

    #[test]
    fn a_broken_hardlink_is_checked_against_the_content_it_was_installed_with() {
        let (_dir, root) = installed();
        // usr/bin/hello2 is installed as a hardlink to usr/bin/hello. Recreated
        // as its own file with the same bytes, it is still intact.
        let same = fs::read(root.join("usr/bin/hello")).unwrap();
        fs::remove_file(root.join("usr/bin/hello2")).unwrap();
        fs::write(root.join("usr/bin/hello2"), &same).unwrap();
        assert_eq!(problems(&root), vec![]);
        // With other bytes, while usr/bin/hello stays intact, it is modified.
        fs::write(root.join("usr/bin/hello2"), "#!/bin/sh\necho changed\n").unwrap();
        assert_eq!(problems(&root), vec![("usr/bin/hello2".into(), Problem::Modified, true)]);
        // A symlink in its place is not the file the package installed.
        fs::remove_file(root.join("usr/bin/hello2")).unwrap();
        symlink("hello", root.join("usr/bin/hello2")).unwrap();
        assert_eq!(problems(&root), vec![("usr/bin/hello2".into(), Problem::Replaced, true)]);
    }

    #[test]
    fn configuration_may_become_a_file_or_symlink_but_nothing_else() {
        let (_dir, root) = installed();
        let conf = root.join("etc/hello.conf");
        let localtime = root.join("etc/localtime");
        let only = |want: Vec<(&str, Problem, bool)>| {
            let want: Vec<(String, Problem, bool)> = want.into_iter().map(|(p, k, f)| (p.to_string(), k, f)).collect();
            assert_eq!(problems(&root), want);
        };
        // A configuration file replaced by a symlink, and a configuration symlink
        // replaced by a copied file: both legitimate.
        fs::remove_file(&conf).unwrap();
        symlink("/run/hello.conf", &conf).unwrap();
        fs::remove_file(&localtime).unwrap();
        fs::write(&localtime, "TZif").unwrap();
        only(vec![
            ("etc/hello.conf", Problem::ModifiedConfig, false),
            ("etc/localtime", Problem::ModifiedConfig, false),
        ]);
        // A directory in place of either is an integrity failure.
        fs::remove_file(&conf).unwrap();
        fs::create_dir(&conf).unwrap();
        fs::remove_file(&localtime).unwrap();
        fs::create_dir(&localtime).unwrap();
        only(vec![("etc/hello.conf", Problem::Replaced, true), ("etc/localtime", Problem::Replaced, true)]);
        // So is a FIFO.
        fs::remove_dir(&conf).unwrap();
        let c = std::ffi::CString::new(conf.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: mkfifo on a valid C string.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o644) }, 0);
        fs::remove_dir(&localtime).unwrap();
        symlink("../usr/share/zoneinfo/UTC", &localtime).unwrap();
        only(vec![("etc/hello.conf", Problem::Replaced, true)]);
    }

    #[test]
    fn unknown_package_is_an_error() {
        let (_dir, root) = installed();
        let db = Db::open(&root).unwrap();
        assert!(verify(&db, &["hello".into()]).unwrap().is_empty());
        assert_eq!(verify(&db, &["nosuch".into()]).unwrap_err(), "nosuch is not installed");
    }
}
