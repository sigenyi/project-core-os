//! Deciding what to install, and in which order.

use std::collections::{BTreeMap, HashSet};

use crate::db::Db;
use crate::repo::{IndexEntry, Repository};

/// A package chosen for installation, with the repository it comes from.
#[derive(Debug, Clone)]
pub struct Planned {
    pub repo: usize,
    pub entry: IndexEntry,
}

fn find<'a>(repos: &'a [Repository], name: &str) -> Option<(usize, &'a IndexEntry)> {
    repos.iter().enumerate().find_map(|(i, r)| r.index.provider(name).map(|e| (i, e)))
}

fn find_library<'a>(repos: &'a [Repository], soname: &str) -> Option<(usize, &'a IndexEntry)> {
    repos.iter().enumerate().find_map(|(i, r)| r.index.library_provider(soname).map(|e| (i, e)))
}

/// Plan the installation of `requested` plus everything they need that is not
/// already installed. With `upgrade`, installed packages with newer versions in a
/// repository are included too. The result is ordered dependencies first.
pub fn plan(
    repos: &[Repository],
    db: &Db,
    requested: &[String],
    upgrade: bool,
    reinstall: bool,
) -> Result<Vec<Planned>, String> {
    let mut picked: BTreeMap<String, Planned> = BTreeMap::new();
    let mut queue: Vec<(String, bool)> = requested.iter().map(|r| (r.clone(), true)).collect();
    if upgrade {
        for (name, inst) in &db.packages {
            if let Some((_, e)) = find(repos, name) {
                if e.version() > inst.manifest.version() {
                    queue.push((name.clone(), true));
                }
            }
        }
    }
    let mut seen = HashSet::new();
    while let Some((name, explicit)) = queue.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let (repo, entry) = find(repos, &name).ok_or_else(|| {
            if explicit {
                format!("no repository has a package named {name}")
            } else {
                format!("dependency {name} is not available")
            }
        })?;
        if let Some(inst) = db.get(&entry.name) {
            let newer = entry.version() > inst.manifest.version();
            if !(explicit && (newer || reinstall) || upgrade && newer) {
                continue; // already installed and current enough
            }
        } else if !explicit && db.satisfies(&name) {
            continue;
        }
        for dep in &entry.depends.packages {
            if !db.satisfies(dep) && !picked.values().any(|p| p.entry.satisfies(dep)) {
                queue.push((dep.clone(), false));
            }
        }
        for lib in &entry.depends.libraries {
            if entry.provides.libraries.contains(lib)
                || db.provides_library(lib).is_some()
                || picked.values().any(|p| p.entry.provides.libraries.contains(lib))
            {
                continue;
            }
            let (_, provider) = find_library(repos, lib)
                .ok_or_else(|| format!("{} needs library {lib}, which no repository provides", entry.name))?;
            queue.push((provider.name.clone(), false));
        }
        picked.insert(entry.name.clone(), Planned { repo, entry: entry.clone() });
    }
    Ok(order(picked))
}

/// Topological order (dependencies first); cycles are broken deterministically.
fn order(picked: BTreeMap<String, Planned>) -> Vec<Planned> {
    let names: Vec<String> = picked.keys().cloned().collect();
    let deps_of = |p: &Planned| -> Vec<String> {
        names
            .iter()
            .filter(|n| **n != p.entry.name)
            .filter(|n| {
                let other = &picked[*n].entry;
                p.entry.depends.packages.iter().any(|d| other.satisfies(d))
                    || p.entry.depends.libraries.iter().any(|l| other.provides.libraries.contains(l))
            })
            .cloned()
            .collect()
    };
    let mut done = HashSet::new();
    let mut visiting = HashSet::new();
    let mut out = Vec::new();
    fn visit(
        n: &str,
        picked: &BTreeMap<String, Planned>,
        deps_of: &dyn Fn(&Planned) -> Vec<String>,
        done: &mut HashSet<String>,
        visiting: &mut HashSet<String>,
        out: &mut Vec<Planned>,
    ) {
        if done.contains(n) || !visiting.insert(n.to_string()) {
            return;
        }
        for d in deps_of(&picked[n]) {
            visit(&d, picked, deps_of, done, visiting, out);
        }
        visiting.remove(n);
        done.insert(n.to_string());
        out.push(picked[n].clone());
    }
    for n in &names {
        visit(n, &picked, &deps_of, &mut done, &mut visiting, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::archive::create_package;
    use crate::archive::tests::manifest;
    use crate::repo::{Index, generate_key, load_signing_key, parse_public_key, write_index};

    /// (name, package deps, needed libraries, provided libraries)
    type Spec<'a> = (&'a str, &'a [&'a str], &'a [&'a str], &'a [&'a str]);

    /// Build a repo of single-file packages with the given deps/libraries.
    fn repo(dir: &Path, specs: &[Spec]) -> Repository {
        for (name, deps, needs_libs, provides_libs) in specs {
            let dest = dir.join(format!("stage-{name}"));
            std::fs::create_dir_all(dest.join("usr/share/doc")).unwrap();
            std::fs::write(dest.join(format!("usr/share/doc/{name}")), name).unwrap();
            let mut m = manifest(name, "1");
            m.depends.packages = deps.iter().map(|s| s.to_string()).collect();
            create_package(&dest, m, dir).unwrap();
            // Library facts are normally detected from ELF files; inject them here.
            let _ = (needs_libs, provides_libs);
        }
        let mut index = Index::scan(dir).unwrap();
        for (name, _, needs, provides) in specs {
            let e = index.packages.iter_mut().find(|p| p.name == *name).unwrap();
            e.depends.libraries = needs.iter().map(|s| s.to_string()).collect();
            e.provides.libraries = provides.iter().map(|s| s.to_string()).collect();
        }
        let (k, p) = generate_key(&dir.join("keys"), "t").unwrap();
        write_index(dir, &index, &load_signing_key(&k).unwrap()).unwrap();
        let trusted = vec![parse_public_key(&std::fs::read_to_string(p).unwrap()).unwrap()];
        Repository::open(dir.to_str().unwrap(), &trusted, true).unwrap()
    }

    #[test]
    fn resolves_package_and_library_dependencies_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let r = repo(
            dir.path(),
            &[
                ("glibc", &[], &[], &["libc.so.6"]),
                ("ncurses", &[], &["libc.so.6"], &["libncursesw.so.6"]),
                ("nano", &["filesystem"], &["libncursesw.so.6", "libc.so.6"], &[]),
                ("filesystem", &[], &[], &[]),
                ("unrelated", &[], &[], &[]),
            ],
        );
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let db = Db::open(&root).unwrap();
        let plan = plan(&[r], &db, &["nano".into()], false, false).unwrap();
        let names: Vec<&str> = plan.iter().map(|p| p.entry.name.as_str()).collect();
        assert_eq!(names.len(), 4);
        let pos = |n: &str| names.iter().position(|x| *x == n).unwrap();
        assert!(pos("glibc") < pos("ncurses") && pos("ncurses") < pos("nano") && pos("filesystem") < pos("nano"));
        assert!(!names.contains(&"unrelated"));
    }

    #[test]
    fn missing_dependencies_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let r = repo(dir.path(), &[("app", &[], &["libgone.so.1"], &[])]);
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let db = Db::open(&root).unwrap();
        let err = plan(&[r], &db, &["app".into()], false, false).unwrap_err();
        assert!(err.contains("libgone.so.1"), "{err}");
    }
}
