//! The `.cpk` package file: a zstd-compressed tar archive.
//!
//! ```text
//! .CORE/manifest.toml   package metadata (always the first entry)
//! .CORE/files           one line per payload entry: kind, mode, size, sha256, path, target
//! usr/...               the payload, relative to the root
//! ```
//!
//! Archives are deterministic: entries are sorted, owners are root and timestamps
//! are the build date, so the same input always yields the same bytes.

use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::elf;
use crate::manifest::{FileEntry, FileKind, Manifest, check_relative_path, format_file_list, parse_file_list};

pub const MANIFEST_ENTRY: &str = ".CORE/manifest.toml";
pub const FILES_ENTRY: &str = ".CORE/files";

/// Top-level locations owned only by the `filesystem` package (merged-/usr layout:
/// `/bin`, `/sbin`, `/lib`, `/lib64` and `/usr/sbin` are symlinks).
const RESERVED_PREFIXES: &[&str] = &["bin", "sbin", "lib", "lib64", "usr/sbin", "usr/lib64"];

pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut f = File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

pub fn sha256_bytes(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// Scan a staged install tree into a sorted file list.
pub fn scan_tree(destdir: &Path) -> Result<Vec<FileEntry>, String> {
    let mut entries = Vec::new();
    // Paths of multiply-linked files and their inode.
    let mut links: HashMap<String, (u64, u64)> = HashMap::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let dir = destdir.join(&rel);
        let mut children: Vec<_> =
            fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?.filter_map(|e| e.ok()).collect();
        children.sort_by_key(|e| e.file_name());
        for child in children {
            let name = child.file_name().to_string_lossy().into_owned();
            let rel_path = if rel.as_os_str().is_empty() { PathBuf::from(&name) } else { rel.join(&name) };
            let path_str = rel_path.to_string_lossy().into_owned();
            check_relative_path(&path_str)?;
            let meta = fs::symlink_metadata(child.path()).map_err(|e| e.to_string())?;
            let ft = meta.file_type();
            let mode = meta.permissions().mode() & 0o7777;
            if ft.is_dir() {
                entries.push(FileEntry {
                    path: path_str,
                    kind: FileKind::Dir,
                    mode,
                    size: 0,
                    sha256: String::new(),
                    target: String::new(),
                });
                stack.push(rel_path);
            } else if ft.is_symlink() {
                let target = fs::read_link(child.path()).map_err(|e| e.to_string())?.to_string_lossy().into_owned();
                entries.push(FileEntry {
                    path: path_str,
                    kind: FileKind::Symlink,
                    mode: 0o777,
                    size: 0,
                    sha256: String::new(),
                    target,
                });
            } else if ft.is_file() {
                if meta.nlink() > 1 {
                    links.insert(path_str.clone(), (meta.dev(), meta.ino()));
                }
                let sha256 = sha256_file(&child.path()).map_err(|e| e.to_string())?;
                entries.push(FileEntry {
                    path: path_str,
                    kind: FileKind::File,
                    mode,
                    size: meta.len(),
                    sha256,
                    target: String::new(),
                });
            } else {
                return Err(format!("{path_str}: device nodes, sockets and FIFOs cannot be packaged"));
            }
        }
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    // Within each group of hard links, the first path in archive order carries the
    // data and the others link to it (an extractor needs the target first).
    let mut first_of: HashMap<(u64, u64), String> = HashMap::new();
    for e in entries.iter_mut() {
        let Some(key) = links.get(&e.path) else { continue };
        match first_of.get(key) {
            None => {
                first_of.insert(*key, e.path.clone());
            }
            Some(first) => {
                e.kind = FileKind::Hardlink;
                e.size = 0;
                e.sha256 = String::new();
                e.target = first.clone();
            }
        }
    }
    Ok(entries)
}

/// Fill in `provides`, library dependencies and size from the payload.
pub fn analyse(destdir: &Path, files: &[FileEntry], manifest: &mut Manifest) -> Result<(), String> {
    let mut binaries = BTreeSet::new();
    let mut sonames = BTreeSet::new();
    let mut needed = BTreeSet::new();
    let mut services = BTreeSet::new();
    let mut config = BTreeSet::new();
    let mut man = BTreeSet::new();
    let mut size = 0u64;
    for f in files {
        size += f.size;
        let p = f.path.as_str();
        if manifest.package.name != "filesystem" {
            if let Some(reserved) = RESERVED_PREFIXES.iter().find(|r| p == **r || p.starts_with(&format!("{r}/"))) {
                return Err(format!(
                    "{p}: packages install into /usr (merged layout); {reserved} belongs to the filesystem package"
                ));
            }
        }
        if let Some(name) = p.strip_prefix("usr/bin/") {
            if !name.contains('/') && (f.kind == FileKind::Symlink || f.mode & 0o111 != 0) {
                binaries.insert(name.to_string());
            }
        }
        for dir in ["usr/lib/systemd/system/", "usr/lib/systemd/user/"] {
            if let Some(unit) = p.strip_prefix(dir) {
                if !unit.contains('/')
                    && [".service", ".socket", ".timer", ".target", ".path", ".mount"].iter().any(|s| unit.ends_with(s))
                {
                    services.insert(unit.to_string());
                }
            }
        }
        if f.is_config() {
            config.insert(p.to_string());
        }
        if let Some(rest) = p.strip_prefix("usr/share/man/") {
            let parts: Vec<&str> = rest.split('/').collect();
            if parts.len() == 2 && parts[0].starts_with("man") && f.kind != FileKind::Dir {
                man.insert(parts[1].trim_end_matches(".gz").trim_end_matches(".xz").to_string());
            }
        }
        if f.kind == FileKind::File && (f.mode & 0o111 != 0 || p.contains(".so")) {
            if let Some(info) = elf::dynamic_info(&destdir.join(p)) {
                needed.extend(info.needed);
                match info.soname {
                    Some(s) => {
                        sonames.insert(s);
                    }
                    // A shared library without a SONAME (perl's libperl.so) is found
                    // by file name, which is what dependents then record as NEEDED.
                    None if info.interpreter.is_none() && p.rsplit('/').next().is_some_and(|n| n.contains(".so")) => {
                        sonames.insert(p.rsplit('/').next().unwrap_or(p).to_string());
                    }
                    None => {}
                }
            }
        }
    }
    let deps: Vec<String> = needed.difference(&sonames).cloned().collect();
    let p = &mut manifest.provides;
    p.binaries = binaries.into_iter().collect();
    p.libraries = sonames.into_iter().collect();
    p.services = services.into_iter().collect();
    p.config = config.into_iter().collect();
    p.man = man.into_iter().collect();
    manifest.depends.libraries = deps;
    manifest.package.installed_size = size;
    Ok(())
}

/// Build a `.cpk` from a staged install tree. Returns the package path.
pub fn create_package(destdir: &Path, mut manifest: Manifest, out_dir: &Path) -> Result<PathBuf, String> {
    manifest.validate()?;
    let files = scan_tree(destdir)?;
    analyse(destdir, &files, &mut manifest)?;
    fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;
    let out = out_dir.join(manifest.file_name());
    let tmp = out.with_extension("cpk.part");
    let mtime = manifest.package.build_date;
    {
        let file = File::create(&tmp).map_err(|e| e.to_string())?;
        let mut enc = zstd::Encoder::new(file, 15).map_err(|e| e.to_string())?;
        let _ = enc.multithread(std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(1));
        let mut tar = tar::Builder::new(enc);
        tar.mode(tar::HeaderMode::Deterministic);
        let add_bytes = |tar: &mut tar::Builder<_>, path: &str, data: &[u8]| -> Result<(), String> {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_mtime(mtime);
            h.set_entry_type(tar::EntryType::Regular);
            tar.append_data(&mut h, path, data).map_err(|e| e.to_string())
        };
        add_bytes(&mut tar, MANIFEST_ENTRY, manifest.to_toml().as_bytes())?;
        add_bytes(&mut tar, FILES_ENTRY, format_file_list(&files).as_bytes())?;
        for f in &files {
            let mut h = tar::Header::new_gnu();
            h.set_mode(f.mode);
            h.set_mtime(mtime);
            h.set_uid(0);
            h.set_gid(0);
            match f.kind {
                FileKind::Dir => {
                    h.set_entry_type(tar::EntryType::Directory);
                    h.set_size(0);
                    tar.append_data(&mut h, format!("{}/", f.path), io::empty()).map_err(|e| e.to_string())?;
                }
                FileKind::Symlink => {
                    h.set_entry_type(tar::EntryType::Symlink);
                    h.set_size(0);
                    tar.append_link(&mut h, &f.path, &f.target).map_err(|e| e.to_string())?;
                }
                FileKind::Hardlink => {
                    h.set_entry_type(tar::EntryType::Link);
                    h.set_size(0);
                    tar.append_link(&mut h, &f.path, &f.target).map_err(|e| e.to_string())?;
                }
                FileKind::File => {
                    h.set_entry_type(tar::EntryType::Regular);
                    h.set_size(f.size);
                    let src = File::open(destdir.join(&f.path)).map_err(|e| format!("{}: {e}", f.path))?;
                    tar.append_data(&mut h, &f.path, src).map_err(|e| format!("{}: {e}", f.path))?;
                }
            }
        }
        let enc = tar.into_inner().map_err(|e| e.to_string())?;
        enc.finish().map_err(|e| e.to_string())?.sync_all().map_err(|e| e.to_string())?;
    }
    fs::rename(&tmp, &out).map_err(|e| e.to_string())?;
    Ok(out)
}

/// Streaming reader over a package's entries.
pub struct PackageReader {
    archive: tar::Archive<zstd::Decoder<'static, io::BufReader<File>>>,
}

pub struct Opened {
    pub manifest: Manifest,
    pub files: Vec<FileEntry>,
}

fn read_entry_string<R: Read>(entry: &mut tar::Entry<R>, max: u64) -> Result<String, String> {
    if entry.header().size().unwrap_or(0) > max {
        return Err("metadata entry too large".into());
    }
    let mut s = String::new();
    entry.read_to_string(&mut s).map_err(|e| e.to_string())?;
    Ok(s)
}

impl PackageReader {
    pub fn open(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let dec = zstd::Decoder::new(file).map_err(|e| e.to_string())?;
        Ok(PackageReader { archive: tar::Archive::new(dec) })
    }

    /// Read the metadata, then call `each` for every payload entry with its contents.
    pub fn read_all(
        &mut self,
        mut each: impl FnMut(&FileEntry, &mut dyn Read) -> Result<(), String>,
        metadata_only: bool,
    ) -> Result<Opened, String> {
        let mut entries = self.archive.entries().map_err(|e| e.to_string())?;
        let mut next = |what: &str| -> Result<tar::Entry<'_, _>, String> {
            entries.next().ok_or_else(|| format!("package is missing {what}"))?.map_err(|e| e.to_string())
        };
        let mut e = next(MANIFEST_ENTRY)?;
        if e.path().map_err(|e| e.to_string())?.to_string_lossy() != MANIFEST_ENTRY {
            return Err("package does not start with a manifest".into());
        }
        let manifest = Manifest::from_toml(&read_entry_string(&mut e, 1 << 20)?)?;
        drop(e);
        let mut e = next(FILES_ENTRY)?;
        if e.path().map_err(|e| e.to_string())?.to_string_lossy() != FILES_ENTRY {
            return Err("package file list missing".into());
        }
        let files = parse_file_list(&read_entry_string(&mut e, 64 << 20)?)?;
        drop(e);
        if metadata_only {
            return Ok(Opened { manifest, files });
        }
        let by_path: HashMap<&str, &FileEntry> = files.iter().map(|f| (f.path.as_str(), f)).collect();
        let mut count = 0;
        for entry in entries {
            let mut entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path().map_err(|e| e.to_string())?.to_string_lossy().trim_end_matches('/').to_string();
            let fe = by_path
                .get(path.as_str())
                .ok_or_else(|| format!("{path} is in the archive but not in the file list"))?;
            each(fe, &mut entry)?;
            count += 1;
        }
        if count != files.len() {
            return Err(format!("archive has {count} entries, file list has {}", files.len()));
        }
        Ok(Opened { manifest, files })
    }
}

/// Manifest and file list only.
pub fn read_metadata(path: &Path) -> Result<Opened, String> {
    PackageReader::open(path)?.read_all(|_, _| Ok(()), true)
}

/// Copy `reader` to `out`, verifying size and SHA-256 against the file list.
pub fn copy_verified(reader: &mut dyn Read, out: &mut dyn Write, entry: &FileEntry) -> Result<(), String> {
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut total = 0u64;
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        total += n as u64;
        h.update(&buf[..n]);
        out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
    }
    if total != entry.size || hex::encode(h.finalize()) != entry.sha256 {
        return Err(format!("{}: content does not match the package's file list", entry.path));
    }
    Ok(())
}

#[cfg(test)]
pub mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    pub fn manifest(name: &str, version: &str) -> Manifest {
        Manifest::from_toml(&format!(
            "[package]\nname = \"{name}\"\nversion = \"{version}\"\nrelease = 1\narch = \"x86_64\"\nsummary = \"test package {name}\"\n"
        ))
        .unwrap()
    }

    pub fn stage(dir: &Path) {
        fs::create_dir_all(dir.join("usr/bin")).unwrap();
        fs::create_dir_all(dir.join("etc")).unwrap();
        fs::create_dir_all(dir.join("usr/share/man/man1")).unwrap();
        fs::create_dir_all(dir.join("usr/lib/systemd/system")).unwrap();
        fs::write(dir.join("usr/bin/hello"), "#!/bin/sh\necho hello\n").unwrap();
        fs::set_permissions(dir.join("usr/bin/hello"), fs::Permissions::from_mode(0o755)).unwrap();
        symlink("hello", dir.join("usr/bin/hi")).unwrap();
        fs::hard_link(dir.join("usr/bin/hello"), dir.join("usr/bin/hello2")).unwrap();
        fs::write(dir.join("etc/hello.conf"), "greeting=hi\n").unwrap();
        fs::write(dir.join("usr/share/man/man1/hello.1"), ".TH HELLO 1\n").unwrap();
        fs::write(dir.join("usr/lib/systemd/system/hello.service"), "[Service]\n").unwrap();
    }

    #[test]
    fn library_without_soname_is_provided_by_file_name() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let lib_dir = d.join("usr/lib/perl5/CORE");
        fs::create_dir_all(&lib_dir).unwrap();
        fs::write(d.join("x.c"), "int x(void) { return 1; }\n").unwrap();
        let built = std::process::Command::new("cc")
            .args(["-shared", "-fPIC", "-o"])
            .arg(lib_dir.join("libperl.so"))
            .arg(d.join("x.c"))
            .status();
        if !built.is_ok_and(|s| s.success()) {
            eprintln!("no C compiler; skipping");
            return;
        }
        fs::remove_file(d.join("x.c")).unwrap();
        let mut m = manifest("perl", "5.40");
        let files = scan_tree(d).unwrap();
        analyse(d, &files, &mut m).unwrap();
        assert!(m.provides.libraries.contains(&"libperl.so".to_string()), "{:?}", m.provides.libraries);
    }

    #[test]
    fn hard_link_groups_follow_archive_order() {
        // glibc installs usr/libexec/getconf/* first and links usr/bin/getconf to
        // one of them; usr/bin sorts first, so it must carry the data.
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        fs::create_dir_all(d.join("usr/libexec/getconf")).unwrap();
        fs::create_dir_all(d.join("usr/bin")).unwrap();
        fs::write(d.join("usr/libexec/getconf/POSIX_V7"), "data").unwrap();
        fs::hard_link(d.join("usr/libexec/getconf/POSIX_V7"), d.join("usr/bin/getconf")).unwrap();
        fs::hard_link(d.join("usr/libexec/getconf/POSIX_V7"), d.join("usr/libexec/getconf/XBS5")).unwrap();
        let files = scan_tree(d).unwrap();
        let get = |p: &str| files.iter().find(|f| f.path == p).unwrap();
        assert_eq!(get("usr/bin/getconf").kind, FileKind::File);
        for p in ["usr/libexec/getconf/POSIX_V7", "usr/libexec/getconf/XBS5"] {
            assert_eq!((get(p).kind, get(p).target.as_str()), (FileKind::Hardlink, "usr/bin/getconf"));
        }
    }

    #[test]
    fn create_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("dest");
        stage(&dest);
        let pkg = create_package(&dest, manifest("hello", "1.0"), &dir.path().join("out")).unwrap();
        assert!(pkg.ends_with("hello-1.0-1.x86_64.cpk"));
        let opened = read_metadata(&pkg).unwrap();
        let p = &opened.manifest.provides;
        assert_eq!(p.binaries, ["hello", "hello2", "hi"]);
        assert_eq!(p.config, ["etc/hello.conf"]);
        assert_eq!(p.man, ["hello.1"]);
        assert_eq!(p.services, ["hello.service"]);
        let hard = opened.files.iter().find(|f| f.path == "usr/bin/hello2").unwrap();
        assert_eq!((hard.kind, hard.target.as_str()), (FileKind::Hardlink, "usr/bin/hello"));

        // Every payload entry verifies against the file list.
        let mut n = 0;
        PackageReader::open(&pkg)
            .unwrap()
            .read_all(
                |fe, r| {
                    n += 1;
                    if fe.kind == FileKind::File {
                        copy_verified(r, &mut io::sink(), fe)?;
                    }
                    Ok(())
                },
                false,
            )
            .unwrap();
        assert_eq!(n, opened.files.len());
    }

    #[test]
    fn packages_are_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("dest");
        stage(&dest);
        let a = create_package(&dest, manifest("hello", "1.0"), &dir.path().join("a")).unwrap();
        let b = create_package(&dest, manifest("hello", "1.0"), &dir.path().join("b")).unwrap();
        assert_eq!(sha256_file(&a).unwrap(), sha256_file(&b).unwrap());
    }

    #[test]
    fn merged_usr_is_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("dest");
        fs::create_dir_all(dest.join("bin")).unwrap();
        fs::write(dest.join("bin/ls"), "x").unwrap();
        let err = create_package(&dest, manifest("bad", "1"), &dir.path().join("out")).unwrap_err();
        assert!(err.contains("merged layout"), "{err}");
    }

    #[test]
    fn library_dependencies_are_detected() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("dest");
        fs::create_dir_all(dest.join("usr/bin")).unwrap();
        let ls = ["/usr/bin/ls", "/bin/ls"].iter().map(Path::new).find(|p| p.exists()).unwrap();
        fs::copy(ls, dest.join("usr/bin/ls")).unwrap();
        let pkg = create_package(&dest, manifest("ls", "1"), &dir.path().join("out")).unwrap();
        let m = read_metadata(&pkg).unwrap().manifest;
        assert!(m.depends.libraries.contains(&"libc.so.6".to_string()), "{:?}", m.depends.libraries);
    }
}
