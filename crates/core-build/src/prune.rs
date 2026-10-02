//! Removing files that no package owns from the build root.
//!
//! The bootstrap leaves temporary tools in the build root, and the final packages
//! replace most of them but not all: a leftover can then shadow a packaged file
//! (a fixincluded header in GCC's search path, a temporary `as` in the cross
//! tooldir that the final GCC searches first). After pruning, the build root
//! holds the installed packages plus local state, so later builds use exactly
//! what the image will contain.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use core_pkg::db::Db;

/// Top-level areas that hold state, mount points or work in progress, never
/// package payload to compare against: left alone.
pub const KEEP: &[&str] = &[
    "build",
    "dev",
    "proc",
    "sys",
    "run",
    "tmp",
    "root",
    "home",
    "etc",
    "var/tmp",
    "var/lib",
    "var/log",
    "var/cache",
    "var/spool",
    "var/mail",
];

#[derive(Debug, Default)]
pub struct Pruned {
    /// Paths (relative to the root) removed, or that would be removed.
    pub removed: Vec<String>,
}

fn kept(rel: &str, keep: &[&str]) -> bool {
    keep.iter().any(|k| rel == *k || rel.starts_with(&format!("{k}/")))
}

/// Remove files, symlinks and then empty directories under `root` that are not in
/// `owned` and not under a `keep` area. Directories count as owned when listed.
pub fn prune_tree(root: &Path, owned: &HashSet<String>, keep: &[&str], dry_run: bool) -> Result<Pruned, String> {
    let mut out = Pruned::default();
    walk(root, root, owned, keep, dry_run, &mut out)?;
    out.removed.sort();
    Ok(out)
}

/// Returns whether `dir` is empty afterwards (or would be).
fn walk(
    root: &Path,
    dir: &Path,
    owned: &HashSet<String>,
    keep: &[&str],
    dry_run: bool,
    out: &mut Pruned,
) -> Result<bool, String> {
    let mut empty = true;
    let mut entries: Vec<_> =
        fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let path = e.path();
        let rel = path.strip_prefix(root).unwrap().to_string_lossy().into_owned();
        if kept(&rel, keep) {
            empty = false;
            continue;
        }
        let meta = fs::symlink_metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if meta.is_dir() {
            let now_empty = walk(root, &path, owned, keep, dry_run, out)?;
            if now_empty && !owned.contains(&rel) {
                if !dry_run {
                    fs::remove_dir(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                }
                out.removed.push(format!("{rel}/"));
            } else {
                empty = false;
            }
        } else if owned.contains(&rel) {
            empty = false;
        } else {
            if !dry_run {
                fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            }
            out.removed.push(rel);
        }
    }
    Ok(empty)
}

/// Prune the build root against its package database.
pub fn prune(root: &Path, dry_run: bool) -> Result<Pruned, String> {
    let db = Db::open(root)?;
    let _lock = db.lock()?;
    // Every path any installed package lists, directories included.
    let all: HashSet<String> = db.packages.values().flat_map(|p| p.files.iter().map(|f| f.path.clone())).collect();
    if all.len() < 1000 {
        return Err(format!("only {} paths are owned by packages; refusing to prune {}", all.len(), root.display()));
    }
    prune_tree(root, &all, KEEP, dry_run)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_only_unowned_outside_kept_areas() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        for d in [
            "usr/bin",
            "usr/x86_64-core-linux-gnu/bin",
            "usr/lib/gcc/include-fixed",
            "etc",
            "build/x",
            "usr/share/empty",
        ] {
            fs::create_dir_all(r.join(d)).unwrap();
        }
        for f in [
            "usr/bin/as",
            "usr/bin/ld",
            "usr/x86_64-core-linux-gnu/bin/as",
            "usr/lib/gcc/include-fixed/pthread.h",
            "etc/hostname",
            "build/x/y",
        ] {
            fs::write(r.join(f), "x").unwrap();
        }
        std::os::unix::fs::symlink("as", r.join("usr/bin/gas")).unwrap();
        let owned: HashSet<String> = ["usr", "usr/bin", "usr/bin/as", "usr/bin/ld", "usr/lib", "usr/lib/gcc"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        let dry = prune_tree(r, &owned, KEEP, true).unwrap();
        assert!(r.join("usr/x86_64-core-linux-gnu/bin/as").exists(), "dry run removes nothing");
        let real = prune_tree(r, &owned, KEEP, false).unwrap();
        assert_eq!(dry.removed, real.removed);
        assert_eq!(
            real.removed,
            [
                "usr/bin/gas",
                "usr/lib/gcc/include-fixed/",
                "usr/lib/gcc/include-fixed/pthread.h",
                "usr/share/",
                "usr/share/empty/",
                "usr/x86_64-core-linux-gnu/",
                "usr/x86_64-core-linux-gnu/bin/",
                "usr/x86_64-core-linux-gnu/bin/as",
            ]
        );
        assert!(r.join("usr/bin/as").exists() && r.join("usr/lib/gcc").is_dir());
        assert!(r.join("etc/hostname").exists() && r.join("build/x/y").exists());
    }
}
