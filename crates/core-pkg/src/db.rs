//! The database of installed packages, under `<root>/var/lib/cpkg/`.
//!
//! ```text
//! installed/<name>/manifest.toml
//! installed/<name>/files
//! history.jsonl        one line per transaction step
//! lock                 held (flock) for the duration of a transaction
//! ```

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::manifest::{FileEntry, FileKind, Manifest, format_file_list, parse_file_list};

pub const DB_DIR: &str = "var/lib/cpkg";

#[derive(Debug, Clone)]
pub struct Installed {
    pub manifest: Manifest,
    pub files: Vec<FileEntry>,
}

pub struct Db {
    root: PathBuf,
    dir: PathBuf,
    pub packages: BTreeMap<String, Installed>,
}

/// An exclusive lock on the database; released when dropped.
pub struct Lock {
    _file: File,
}

impl Db {
    pub fn open(root: &Path) -> Result<Db, String> {
        let dir = root.join(DB_DIR);
        let mut packages = BTreeMap::new();
        let installed = dir.join("installed");
        if let Ok(rd) = fs::read_dir(&installed) {
            for e in rd.filter_map(|e| e.ok()) {
                let p = e.path();
                if p.extension().is_some() {
                    continue; // half-written entries (.tmp) from an interrupted run
                }
                let manifest = fs::read_to_string(p.join("manifest.toml"))
                    .map_err(|e| format!("{}: {e}", p.display()))
                    .and_then(|t| Manifest::from_toml(&t))?;
                let files = fs::read_to_string(p.join("files"))
                    .map_err(|e| format!("{}: {e}", p.display()))
                    .and_then(|t| parse_file_list(&t))?;
                packages.insert(manifest.package.name.clone(), Installed { manifest, files });
            }
        }
        Ok(Db { root: root.to_path_buf(), dir, packages })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn lock(&self) -> Result<Lock, String> {
        fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.dir.join("lock"))
            .map_err(|e| e.to_string())?;
        // SAFETY: flock on a file descriptor we own.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("another cpkg transaction is running".into());
        }
        Ok(Lock { _file: file })
    }

    pub fn get(&self, name: &str) -> Option<&Installed> {
        self.packages.get(name)
    }

    /// Owner of each non-directory path.
    pub fn owners(&self) -> HashMap<&str, &str> {
        let mut map = HashMap::new();
        for (name, pkg) in &self.packages {
            for f in pkg.files.iter().filter(|f| f.kind != FileKind::Dir) {
                map.insert(f.path.as_str(), name.as_str());
            }
        }
        map
    }

    /// Packages owning a directory path.
    pub fn dir_users(&self, path: &str) -> Vec<&str> {
        self.packages
            .iter()
            .filter(|(_, p)| p.files.iter().any(|f| f.kind == FileKind::Dir && f.path == path))
            .map(|(n, _)| n.as_str())
            .collect()
    }

    /// Whether some installed package satisfies `name` (by name or provided name).
    pub fn satisfies(&self, name: &str) -> bool {
        self.packages.contains_key(name)
            || self.packages.values().any(|p| p.manifest.provides.names.iter().any(|n| n == name))
    }

    pub fn provides_library(&self, soname: &str) -> Option<&str> {
        self.packages
            .iter()
            .find(|(_, p)| p.manifest.provides.libraries.iter().any(|l| l == soname))
            .map(|(n, _)| n.as_str())
    }

    /// Installed packages that need `name` (directly by name, or a library only it provides).
    pub fn dependents(&self, name: &str) -> Vec<String> {
        let Some(target) = self.packages.get(name) else { return Vec::new() };
        let libs = &target.manifest.provides.libraries;
        self.packages
            .iter()
            .filter(|(n, _)| n.as_str() != name)
            .filter(|(_, p)| {
                p.manifest.depends.packages.iter().any(|d| d == name || target.manifest.provides.names.contains(d))
                    || p.manifest.depends.libraries.iter().any(|l| {
                        libs.contains(l)
                            && !self
                                .packages
                                .iter()
                                .any(|(o, op)| o != name && op.manifest.provides.libraries.contains(l))
                    })
            })
            .map(|(n, _)| n.clone())
            .collect()
    }

    pub fn record(&mut self, manifest: &Manifest, files: &[FileEntry]) -> Result<(), String> {
        let installed = self.dir.join("installed");
        let final_dir = installed.join(&manifest.package.name);
        let tmp = installed.join(format!("{}.tmp", manifest.package.name));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
        fs::write(tmp.join("manifest.toml"), manifest.to_toml()).map_err(|e| e.to_string())?;
        fs::write(tmp.join("files"), format_file_list(files)).map_err(|e| e.to_string())?;
        let old = installed.join(format!("{}.old", manifest.package.name));
        let _ = fs::remove_dir_all(&old);
        if final_dir.exists() {
            fs::rename(&final_dir, &old).map_err(|e| e.to_string())?;
        }
        fs::rename(&tmp, &final_dir).map_err(|e| e.to_string())?;
        let _ = fs::remove_dir_all(&old);
        self.packages
            .insert(manifest.package.name.clone(), Installed { manifest: manifest.clone(), files: files.to_vec() });
        Ok(())
    }

    pub fn forget(&mut self, name: &str) -> Result<(), String> {
        let dir = self.dir.join("installed").join(name);
        if dir.exists() {
            fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
        }
        self.packages.remove(name);
        Ok(())
    }

    pub fn log(&self, op: &str, name: &str, from: Option<&str>, to: Option<&str>) {
        let line = json!({"ts": core_protocol::time::now_rfc3339(), "op": op, "package": name, "from": from, "to": to});
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(self.dir.join("history.jsonl")) {
            let _ = writeln!(f, "{line}");
        }
    }

    pub fn history(&self) -> Vec<serde_json::Value> {
        fs::read_to_string(self.dir.join("history.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }
}
