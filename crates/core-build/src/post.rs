//! Post-processing of a staged install tree before it is packaged.
//!
//! * merged `/usr`: anything installed into `/bin`, `/sbin`, `/lib`, `/lib64`,
//!   `/usr/sbin` or `/usr/lib64` is moved into `/usr/bin` or `/usr/lib`
//! * libtool archives (`.la`) and the info directory index are removed
//! * ELF files are stripped of symbols they do not need at run time

use std::fs;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Component, Path, PathBuf};

use core_pkg::elf;

use crate::env::command_in_root;

const MOVES: &[(&str, &str)] = &[
    ("bin", "usr/bin"),
    ("sbin", "usr/bin"),
    ("usr/sbin", "usr/bin"),
    ("lib", "usr/lib"),
    ("lib64", "usr/lib"),
    ("usr/lib64", "usr/lib"),
];

fn same_content(a: &Path, b: &Path) -> bool {
    match (fs::symlink_metadata(a), fs::symlink_metadata(b)) {
        (Ok(ma), Ok(mb)) if ma.file_type().is_symlink() && mb.file_type().is_symlink() => {
            fs::read_link(a).ok() == fs::read_link(b).ok()
        }
        (Ok(ma), Ok(mb)) if ma.is_file() && mb.is_file() => {
            ma.ino() == mb.ino() && ma.dev() == mb.dev() || fs::read(a).ok() == fs::read(b).ok()
        }
        _ => false,
    }
}

/// Lexically resolve a symlink target relative to the link's directory.
fn resolve_lexical(link: &Path, target: &Path) -> PathBuf {
    let mut out =
        if target.is_absolute() { PathBuf::from("/") } else { link.parent().unwrap_or(Path::new("")).to_path_buf() };
    for c in target.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(n) => out.push(n),
            _ => {}
        }
    }
    out
}

fn merge_dir(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    for entry in fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let from_meta = fs::symlink_metadata(&from).map_err(|e| e.to_string())?;
        match fs::symlink_metadata(&to) {
            Err(_) => fs::rename(&from, &to).map_err(|e| format!("{} -> {}: {e}", from.display(), to.display()))?,
            Ok(to_meta) if to_meta.is_dir() && from_meta.is_dir() => merge_dir(&from, &to)?,
            Ok(_) if same_content(&from, &to) => fs::remove_file(&from).map_err(|e| e.to_string())?,
            Ok(_) if from_meta.file_type().is_symlink() => {
                // e.g. sbin/foo -> ../bin/foo, which becomes the file itself after merging.
                let target = fs::read_link(&from).map_err(|e| e.to_string())?;
                let points_to = resolve_lexical(&from, &target);
                if points_to.file_name() == to.file_name() {
                    fs::remove_file(&from).map_err(|e| e.to_string())?;
                } else {
                    return Err(format!("{} and {} conflict after merging /usr", from.display(), to.display()));
                }
            }
            Ok(_) => return Err(format!("{} and {} conflict after merging /usr", from.display(), to.display())),
        }
    }
    fs::remove_dir(src).map_err(|e| format!("{}: {e}", src.display()))
}

/// Rewrite symlink targets that mention the merged directories.
fn fix_symlinks(dir: &Path) -> Result<(), String> {
    let Ok(rd) = fs::read_dir(dir) else { return Ok(()) };
    for entry in rd.filter_map(|e| e.ok()) {
        let p = entry.path();
        let meta = fs::symlink_metadata(&p).map_err(|e| e.to_string())?;
        if meta.is_dir() {
            fix_symlinks(&p)?;
        } else if meta.file_type().is_symlink() {
            let target = fs::read_link(&p).map_err(|e| e.to_string())?.to_string_lossy().into_owned();
            let mut fixed = target.clone();
            for (from, to) in [
                ("/usr/sbin/", "/usr/bin/"),
                ("/usr/lib64/", "/usr/lib/"),
                ("../sbin/", "../bin/"),
                ("../lib64/", "../lib/"),
            ] {
                fixed = fixed.replace(from, to);
            }
            for (from, to) in
                [("/sbin/", "/usr/bin/"), ("/bin/", "/usr/bin/"), ("/lib64/", "/usr/lib/"), ("/lib/", "/usr/lib/")]
            {
                if fixed.starts_with(from) {
                    fixed = format!("{to}{}", &fixed[from.len()..]);
                }
            }
            if fixed != target {
                fs::remove_file(&p).map_err(|e| e.to_string())?;
                symlink(&fixed, &p).map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}

pub fn normalize_merged_usr(dest: &Path) -> Result<(), String> {
    for (from, to) in MOVES {
        let src = dest.join(from);
        match fs::symlink_metadata(&src) {
            Ok(m) if m.is_dir() => merge_dir(&src, &dest.join(to))?,
            Ok(m) if m.file_type().is_symlink() => fs::remove_file(&src).map_err(|e| e.to_string())?,
            _ => {}
        }
    }
    fix_symlinks(dest)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            match fs::symlink_metadata(&p) {
                Ok(m) if m.is_dir() => walk(&p, out),
                Ok(m) if m.is_file() => out.push(p),
                _ => {}
            }
        }
    }
}

pub fn remove_clutter(dest: &Path) -> Result<(), String> {
    let mut files = Vec::new();
    walk(dest, &mut files);
    for f in files {
        let name = f.file_name().unwrap().to_string_lossy();
        let rel = f.strip_prefix(dest).unwrap();
        if name.ends_with(".la") && rel.starts_with("usr/lib") {
            fs::remove_file(&f).map_err(|e| e.to_string())?;
        }
    }
    let info_dir = dest.join("usr/share/info/dir");
    if info_dir.exists() {
        fs::remove_file(info_dir).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Strip ELF files in `dest` with the build root's own `strip` (run chrooted into
/// `root`, which contains `dest`), so the binutils that built them also strips them.
/// Returns the number of files handed to strip.
pub fn strip(dest: &Path, root: &Path) -> Result<usize, String> {
    let rel_dest =
        dest.strip_prefix(root).map_err(|_| format!("{} is not inside {}", dest.display(), root.display()))?;
    let mut files = Vec::new();
    walk(dest, &mut files);
    let mut debug_only = Vec::new();
    let mut unneeded = Vec::new();
    for f in files {
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        let inside = Path::new("/").join(rel_dest).join(f.strip_prefix(dest).unwrap());
        if name.ends_with(".a") {
            if fs::read(&f).map(|b| b.starts_with(b"!<arch>")).unwrap_or(false) {
                debug_only.push(inside);
            }
        } else if !elf::is_elf(&f) || name.ends_with(".o") {
            continue;
        } else if name.ends_with(".ko") || name.starts_with("ld-linux") || name.starts_with("libc.so") {
            // Kernel modules need their symbols; the dynamic loader and libc keep
            // theirs for debuggers and sanitizers.
            debug_only.push(inside);
        } else {
            unneeded.push(inside);
        }
    }
    let count = debug_only.len() + unneeded.len();
    for (flag, list) in [("--strip-debug", debug_only), ("--strip-unneeded", unneeded)] {
        for chunk in list.chunks(200) {
            // strip processes every file even when some fail (e.g. a script named .a).
            let out = command_in_root(root, "/usr/bin/strip")?
                .arg(flag)
                .args(chunk)
                .output()
                .map_err(|e| format!("cannot run strip: {e}"))?;
            if !out.status.success() {
                log::debug!("strip: {}", String::from_utf8_lossy(&out.stderr).trim());
            }
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_usr() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        fs::create_dir_all(d.join("bin")).unwrap();
        fs::create_dir_all(d.join("sbin")).unwrap();
        fs::create_dir_all(d.join("usr/bin")).unwrap();
        fs::create_dir_all(d.join("lib64")).unwrap();
        fs::write(d.join("bin/ls"), "ls").unwrap();
        fs::write(d.join("sbin/fsck"), "fsck").unwrap();
        fs::write(d.join("usr/bin/ls"), "ls").unwrap(); // identical duplicate
        fs::write(d.join("usr/bin/mkfs"), "mkfs").unwrap();
        fs::create_dir_all(d.join("usr/sbin")).unwrap();
        symlink("../bin/mkfs", d.join("usr/sbin/mkfs")).unwrap(); // self-reference after merge
        symlink("/sbin/fsck", d.join("usr/bin/fsck.alias")).unwrap();
        fs::write(d.join("lib64/libx.so.1"), "x").unwrap();
        normalize_merged_usr(d).unwrap();
        assert!(!d.join("bin").exists() && !d.join("sbin").exists() && !d.join("usr/sbin").exists());
        assert_eq!(fs::read_to_string(d.join("usr/bin/fsck")).unwrap(), "fsck");
        assert_eq!(fs::read_to_string(d.join("usr/bin/mkfs")).unwrap(), "mkfs");
        assert_eq!(fs::read_to_string(d.join("usr/lib/libx.so.1")).unwrap(), "x");
        assert_eq!(fs::read_link(d.join("usr/bin/fsck.alias")).unwrap(), Path::new("/usr/bin/fsck"));
    }

    #[test]
    fn conflicting_files_are_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        fs::create_dir_all(d.join("bin")).unwrap();
        fs::create_dir_all(d.join("usr/bin")).unwrap();
        fs::write(d.join("bin/x"), "a").unwrap();
        fs::write(d.join("usr/bin/x"), "b").unwrap();
        assert!(normalize_merged_usr(d).unwrap_err().contains("conflict"));
    }

    #[test]
    fn clutter() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        fs::create_dir_all(d.join("usr/lib")).unwrap();
        fs::create_dir_all(d.join("usr/share/info")).unwrap();
        fs::write(d.join("usr/lib/libx.la"), "libtool").unwrap();
        fs::write(d.join("usr/lib/libx.so.1"), "keep").unwrap();
        fs::write(d.join("usr/share/info/dir"), "index").unwrap();
        remove_clutter(d).unwrap();
        assert!(!d.join("usr/lib/libx.la").exists() && !d.join("usr/share/info/dir").exists());
        assert!(d.join("usr/lib/libx.so.1").exists());
    }
}
