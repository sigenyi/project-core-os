//! Installing, upgrading and removing packages.
//!
//! An install runs in three phases so that a failure leaves the system unchanged:
//!
//! 1. **stage**: every file is written next to its destination under a temporary
//!    name, verified against the package's checksums;
//! 2. **commit**: temporary files are renamed into place (atomic per file);
//! 3. **clean up**: on upgrade, files the new version no longer ships are removed.
//!
//! Configuration files (`etc/…`) changed by the user are never overwritten: the new
//! version is written as `<file>.cpknew`. Removing a package keeps changed
//! configuration as `<file>.cpksave`.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::archive::{PackageReader, copy_verified, sha256_file};
use crate::db::Db;
use crate::hooks;
use crate::manifest::{FileEntry, FileKind};

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Allow replacing files that exist on disk but belong to no package (used when
    /// final packages replace a bootstrap's temporary tools).
    pub overwrite_unowned: bool,
    /// Remove packages even if others depend on them.
    pub force: bool,
    pub run_hooks: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Report {
    pub installed: Vec<Change>,
    pub removed: Vec<Change>,
    /// Notes for the user (kept configuration, hook failures).
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Change {
    pub name: String,
    pub from: Option<String>,
    pub to: Option<String>,
}

fn io_err(path: &Path) -> impl Fn(io::Error) -> String + '_ {
    move |e| format!("{}: {e}", path.display())
}

/// Create the parent of `target` and make sure it resolves inside `root`.
fn prepare_parent(root: &Path, target: &Path) -> Result<(), String> {
    let parent = target.parent().ok_or("path has no parent")?;
    fs::create_dir_all(parent).map_err(io_err(parent))?;
    let canon_root = fs::canonicalize(root).map_err(io_err(root))?;
    let canon_parent = fs::canonicalize(parent).map_err(io_err(parent))?;
    if !canon_parent.starts_with(&canon_root) {
        return Err(format!("{} escapes the installation root", target.display()));
    }
    Ok(())
}

fn temp_name(target: &Path) -> PathBuf {
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    target.with_file_name(format!(".{name}.cpk-new"))
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// Install or upgrade one package from a verified local file.
pub fn install_package(
    db: &mut Db,
    package: &Path,
    opts: &Options,
    report: &mut Report,
    changed: &mut Vec<String>,
) -> Result<(), String> {
    let root = db.root().to_path_buf();
    let meta = crate::archive::read_metadata(package)?;
    let name = meta.manifest.package.name.clone();
    let old = db.get(&name).cloned();

    // Conflicts are checked before anything is written.
    let owners = db.owners();
    for f in meta.files.iter().filter(|f| f.kind != FileKind::Dir) {
        let target = root.join(&f.path);
        match owners.get(f.path.as_str()) {
            Some(owner) if *owner != name => {
                return Err(format!("{name}: /{} is already owned by package {owner}", f.path));
            }
            Some(_) => {}
            None => {
                if let Ok(m) = fs::symlink_metadata(&target) {
                    if m.is_dir() {
                        return Err(format!("{name}: /{} exists as a directory", f.path));
                    }
                    if !f.is_config() && !opts.overwrite_unowned {
                        return Err(format!("{name}: /{} exists but belongs to no package", f.path));
                    }
                }
            }
        }
    }

    // Phase 1: stage.
    let mut staged: Vec<(PathBuf, PathBuf, FileEntry)> = Vec::new();
    let mut staged_by_path: HashMap<String, PathBuf> = HashMap::new();
    let result = PackageReader::open(package)?.read_all(
        |fe, reader| {
            let target = root.join(&fe.path);
            match fe.kind {
                FileKind::Dir => {
                    prepare_parent(&root, &target)?;
                    match fs::symlink_metadata(&target) {
                        Ok(m) if m.is_dir() || m.file_type().is_symlink() => {}
                        Ok(_) => return Err(format!("/{} exists and is not a directory", fe.path)),
                        Err(_) => fs::create_dir(&target).map_err(io_err(&target))?,
                    }
                    if !fs::symlink_metadata(&target).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
                        fs::set_permissions(&target, fs::Permissions::from_mode(fe.mode)).map_err(io_err(&target))?;
                    }
                    return Ok(());
                }
                FileKind::File => {
                    prepare_parent(&root, &target)?;
                    let tmp = temp_name(&target);
                    let mut out = File::create(&tmp).map_err(io_err(&tmp))?;
                    copy_verified(reader, &mut out, fe)?;
                    out.sync_all().map_err(io_err(&tmp))?;
                    fs::set_permissions(&tmp, fs::Permissions::from_mode(fe.mode)).map_err(io_err(&tmp))?;
                    staged_by_path.insert(fe.path.clone(), tmp.clone());
                    staged.push((tmp, target, fe.clone()));
                }
                FileKind::Symlink => {
                    prepare_parent(&root, &target)?;
                    let tmp = temp_name(&target);
                    let _ = fs::remove_file(&tmp);
                    symlink(&fe.target, &tmp).map_err(io_err(&tmp))?;
                    staged.push((tmp, target, fe.clone()));
                }
                FileKind::Hardlink => {
                    prepare_parent(&root, &target)?;
                    let source = staged_by_path
                        .get(&fe.target)
                        .ok_or_else(|| format!("hard link {} points at unknown {}", fe.path, fe.target))?;
                    let tmp = temp_name(&target);
                    let _ = fs::remove_file(&tmp);
                    fs::hard_link(source, &tmp).map_err(io_err(&tmp))?;
                    staged.push((tmp, target, fe.clone()));
                }
            }
            Ok(())
        },
        false,
    );
    if let Err(e) = result {
        for (tmp, _, _) in &staged {
            let _ = fs::remove_file(tmp);
        }
        return Err(format!("{name}: {e}"));
    }

    // Phase 2: commit.
    let old_hashes: HashMap<&str, &str> = old
        .as_ref()
        .map(|o| o.files.iter().map(|f| (f.path.as_str(), f.sha256.as_str())).collect())
        .unwrap_or_default();
    for (tmp, target, fe) in &staged {
        if fe.is_config() && exists(target) {
            let on_disk = sha256_file(target).unwrap_or_default();
            if on_disk == fe.sha256 {
                let _ = fs::remove_file(tmp);
                continue;
            }
            let unmodified = old_hashes.get(fe.path.as_str()).is_some_and(|h| *h == on_disk);
            if !unmodified {
                let keep = PathBuf::from(format!("{}.cpknew", target.display()));
                fs::rename(tmp, &keep).map_err(io_err(&keep))?;
                report.notes.push(format!("/{} was changed locally; the new version is /{}.cpknew", fe.path, fe.path));
                continue;
            }
        }
        fs::rename(tmp, target).map_err(io_err(target))?;
        changed.push(fe.path.clone());
    }

    // Phase 3: files dropped by the new version.
    if let Some(old) = &old {
        let new_paths: HashSet<&str> = meta.files.iter().map(|f| f.path.as_str()).collect();
        let obsolete: Vec<FileEntry> =
            old.files.iter().filter(|f| !new_paths.contains(f.path.as_str())).cloned().collect();
        remove_files(db, &name, &obsolete, report, changed);
    }

    let from = old.as_ref().map(|o| o.manifest.id());
    db.record(&meta.manifest, &meta.files)?;
    db.log(if from.is_some() { "upgrade" } else { "install" }, &name, from.as_deref(), Some(&meta.manifest.id()));
    report.installed.push(Change { name, from, to: Some(meta.manifest.id()) });
    Ok(())
}

/// Delete a package's files (deepest first), keeping modified configuration and
/// directories still used by other packages.
fn remove_files(db: &Db, name: &str, files: &[FileEntry], report: &mut Report, changed: &mut Vec<String>) {
    let root = db.root();
    let mut sorted: Vec<&FileEntry> = files.iter().collect();
    sorted.sort_by(|a, b| b.path.cmp(&a.path));
    let owners = db.owners();
    for f in sorted {
        let target = root.join(&f.path);
        if f.kind == FileKind::Dir {
            if db.dir_users(&f.path).iter().all(|u| *u == name) {
                let _ = fs::remove_dir(&target); // only succeeds when empty
            }
            continue;
        }
        if owners.get(f.path.as_str()).is_some_and(|o| *o != name) {
            continue; // taken over by another package
        }
        if f.is_config() && exists(&target) && sha256_file(&target).ok().as_deref() != Some(f.sha256.as_str()) {
            let save = PathBuf::from(format!("{}.cpksave", target.display()));
            if fs::rename(&target, &save).is_ok() {
                report.notes.push(format!("kept your changes to /{} as /{}.cpksave", f.path, f.path));
            }
            continue;
        }
        if fs::remove_file(&target).is_ok() {
            changed.push(f.path.clone());
        }
    }
}

pub fn remove_package(
    db: &mut Db,
    name: &str,
    removing: &[String],
    opts: &Options,
    report: &mut Report,
    changed: &mut Vec<String>,
) -> Result<(), String> {
    let installed = db.get(name).cloned().ok_or_else(|| format!("{name} is not installed"))?;
    let dependents: Vec<String> = db.dependents(name).into_iter().filter(|d| !removing.contains(d)).collect();
    if !dependents.is_empty() && !opts.force {
        return Err(format!("{name} is needed by {}", dependents.join(", ")));
    }
    remove_files(db, name, &installed.files, report, changed);
    db.forget(name)?;
    db.log("remove", name, Some(&installed.manifest.id()), None);
    report.removed.push(Change { name: name.to_string(), from: Some(installed.manifest.id()), to: None });
    Ok(())
}

/// Run hooks triggered by the changed paths.
pub fn run_hooks(db: &Db, changed: &[String], report: &mut Report) {
    let all = hooks::load(db.root());
    for hook in hooks::triggered(&all, changed) {
        if let Err(e) = hooks::run(hook, db.root()) {
            report.notes.push(e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::create_package;
    use crate::archive::tests::{manifest, stage};

    fn build(dir: &Path, name: &str, version: &str, tweak: impl Fn(&Path)) -> PathBuf {
        let dest = dir.join(format!("stage-{name}-{version}"));
        stage(&dest);
        tweak(&dest);
        let pkg = create_package(&dest, manifest(name, version), &dir.join("pkgs")).unwrap();
        fs::remove_dir_all(&dest).unwrap();
        pkg
    }

    fn install(db: &mut Db, pkg: &Path, opts: &Options) -> Result<Report, String> {
        let mut report = Report::default();
        let mut changed = Vec::new();
        install_package(db, pkg, opts, &mut report, &mut changed)?;
        Ok(report)
    }

    #[test]
    fn install_upgrade_remove() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir_all(&root).unwrap();
        let v1 = build(dir.path(), "hello", "1.0", |_| {});
        let v2 = build(dir.path(), "hello", "2.0", |d| {
            fs::write(d.join("usr/bin/hello"), "#!/bin/sh\necho v2\n").unwrap();
            fs::remove_file(d.join("usr/share/man/man1/hello.1")).unwrap();
            fs::write(d.join("etc/hello.conf"), "greeting=v2\n").unwrap();
        });
        let mut db = Db::open(&root).unwrap();
        install(&mut db, &v1, &Options::default()).unwrap();
        assert_eq!(fs::read_to_string(root.join("usr/bin/hello2")).unwrap(), "#!/bin/sh\necho hello\n");
        assert_eq!(fs::read_link(root.join("usr/bin/hi")).unwrap(), Path::new("hello"));
        assert_eq!(fs::metadata(root.join("usr/bin/hello")).unwrap().permissions().mode() & 0o777, 0o755);

        // The user edits the config; the upgrade must keep it.
        fs::write(root.join("etc/hello.conf"), "greeting=mine\n").unwrap();
        let report = install(&mut Db::open(&root).unwrap(), &v2, &Options::default()).unwrap();
        assert_eq!(report.installed[0].from.as_deref(), Some("hello-1.0-1"));
        assert_eq!(fs::read_to_string(root.join("usr/bin/hello")).unwrap(), "#!/bin/sh\necho v2\n");
        assert_eq!(fs::read_to_string(root.join("etc/hello.conf")).unwrap(), "greeting=mine\n");
        assert_eq!(fs::read_to_string(root.join("etc/hello.conf.cpknew")).unwrap(), "greeting=v2\n");
        assert!(!root.join("usr/share/man/man1/hello.1").exists(), "obsolete file removed");

        let mut db = Db::open(&root).unwrap();
        let mut report = Report::default();
        remove_package(&mut db, "hello", &[], &Options::default(), &mut report, &mut Vec::new()).unwrap();
        assert!(!root.join("usr/bin/hello").exists());
        assert!(root.join("etc/hello.conf.cpksave").exists(), "modified config kept");
        assert!(Db::open(&root).unwrap().packages.is_empty());
        assert!(!root.join("usr/lib/systemd/system").exists(), "empty directories cleaned up");
    }

    #[test]
    fn conflicts_and_unowned_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir_all(root.join("usr/bin")).unwrap();
        let a = build(dir.path(), "a", "1", |_| {});
        let b = build(dir.path(), "b", "1", |_| {});
        let mut db = Db::open(&root).unwrap();
        install(&mut db, &a, &Options::default()).unwrap();
        let err = install(&mut db, &b, &Options::default()).unwrap_err();
        assert!(err.contains("already owned by package a"), "{err}");

        // A file left by a bootstrap is only replaced when explicitly allowed.
        let root2 = dir.path().join("root2");
        fs::create_dir_all(root2.join("usr/bin")).unwrap();
        fs::write(root2.join("usr/bin/hello"), "temporary tool").unwrap();
        let mut db2 = Db::open(&root2).unwrap();
        assert!(install(&mut db2, &a, &Options::default()).unwrap_err().contains("belongs to no package"));
        assert!(!root2.join("usr/bin/hi").exists(), "nothing written on failure");
        install(&mut db2, &a, &Options { overwrite_unowned: true, ..Default::default() }).unwrap();
        assert_ne!(fs::read_to_string(root2.join("usr/bin/hello")).unwrap(), "temporary tool");
    }

    #[test]
    fn corrupted_packages_change_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir_all(&root).unwrap();
        let pkg = build(dir.path(), "hello", "1.0", |_| {});
        let mut bytes = fs::read(&pkg).unwrap();
        let n = bytes.len();
        bytes.truncate(n - 64);
        fs::write(&pkg, bytes).unwrap();
        let mut db = Db::open(&root).unwrap();
        assert!(install(&mut db, &pkg, &Options::default()).is_err());
        assert!(Db::open(&root).unwrap().packages.is_empty());
        assert!(!root.join("usr/bin/hello").exists());
    }

    #[test]
    fn removal_respects_dependents() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir_all(&root).unwrap();
        let base = build(dir.path(), "base", "1", |_| {});
        let dest = dir.path().join("stage-app");
        fs::create_dir_all(dest.join("usr/bin")).unwrap();
        fs::write(dest.join("usr/bin/app"), "x").unwrap();
        let mut m = manifest("app", "1");
        m.depends.packages.push("base".into());
        let app = create_package(&dest, m, &dir.path().join("pkgs")).unwrap();
        let mut db = Db::open(&root).unwrap();
        install(&mut db, &base, &Options::default()).unwrap();
        install(&mut db, &app, &Options::default()).unwrap();
        let mut report = Report::default();
        let err = remove_package(&mut db, "base", &[], &Options::default(), &mut report, &mut Vec::new()).unwrap_err();
        assert!(err.contains("needed by app"), "{err}");
        remove_package(&mut db, "base", &["app".into()], &Options::default(), &mut report, &mut Vec::new()).unwrap();
    }
}
