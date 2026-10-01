//! `read_file` and `list_directory`, performed by the agent with the user's own
//! permissions.
//!
//! Reading needs no privilege, so it never happens in the root Guardian: the kernel
//! decides what the user may read. On top of that, a [`PathPolicy`] keeps credentials
//! the user *can* read out of the model's context. Opens are non-blocking and refuse
//! to follow a symlink in the last component, and the type of what was actually opened
//! is checked with `fstat` (FIFOs, devices and sockets are refused), so a read can
//! neither hang nor be redirected after the policy check.

use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use core_protocol::paths::PathPolicy;

const MAX_DIR_ENTRIES: usize = 200;
const MAX_READ_BYTES: u64 = 256 * 1024;

fn open_checked(path: &Path, policy: &PathPolicy, directory: bool) -> Result<(File, PathBuf), String> {
    let canon = fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
    policy.check_readable(&canon)?;
    let mut flags = libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NOCTTY;
    if directory {
        flags |= libc::O_DIRECTORY;
    }
    let file = OpenOptions::new().read(true).custom_flags(flags).open(&canon).map_err(|e| match e.raw_os_error() {
        Some(libc::ENOTDIR) => format!("{} is not a directory", canon.display()),
        Some(libc::ELOOP) => format!("{} changed while being opened", canon.display()),
        _ => format!("{}: {e}", canon.display()),
    })?;
    let file_type = file.metadata().map_err(|e| e.to_string())?.file_type();
    if directory && !file_type.is_dir() {
        return Err(format!("{} is not a directory", canon.display()));
    }
    if !directory {
        if file_type.is_dir() {
            return Err(format!("{} is a directory; use list_directory", canon.display()));
        }
        if !file_type.is_file() {
            return Err(format!("{} is not a regular file", canon.display()));
        }
    }
    Ok((file, canon))
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{v:.1} {}", UNITS[unit]) }
}

/// List a directory's entries (directories get a trailing `/`, symlinks show targets).
pub fn list_directory(path: &Path, policy: &PathPolicy) -> Result<String, String> {
    let (dir, canon) = open_checked(path, policy, true)?;
    // Enumerate the directory that was actually opened, not whatever the path names now.
    let handle = PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd()));
    let source = if handle.exists() { handle } else { canon.clone() };
    let mut entries: Vec<(String, String)> = fs::read_dir(&source)
        .map_err(|e| format!("{}: {e}", canon.display()))?
        .filter_map(|e| e.ok())
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let line = match e.file_type() {
                Ok(t) if t.is_dir() => format!("{name}/"),
                Ok(t) if t.is_symlink() => {
                    let target = fs::read_link(e.path()).map(|t| t.display().to_string()).unwrap_or_default();
                    format!("{name} -> {target}")
                }
                Ok(_) => match e.metadata() {
                    Ok(m) => format!("{name}  {}", human_size(m.len())),
                    Err(_) => name.clone(),
                },
                Err(_) => name.clone(),
            };
            (name, line)
        })
        .collect();
    entries.sort();
    let total = entries.len();
    let mut out: Vec<String> = entries.into_iter().take(MAX_DIR_ENTRIES).map(|(_, l)| l).collect();
    if total > MAX_DIR_ENTRIES {
        out.push(format!("... and {} more entries", total - MAX_DIR_ENTRIES));
    }
    if total == 0 {
        out.push("(empty directory)".into());
    }
    Ok(format!("{}:\n{}", canon.display(), out.join("\n")))
}

/// Read up to `lines` lines from the start (or end, with `tail`) of a text file.
pub fn read_file(path: &Path, lines: usize, tail: bool, policy: &PathPolicy) -> Result<String, String> {
    let (mut file, canon) = open_checked(path, policy, false)?;
    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
    // procfs/sysfs report size 0, so always read with a cap; for large files read
    // only the end that was asked for.
    let mut skipped_prefix = false;
    if tail && size > MAX_READ_BYTES {
        file.seek(SeekFrom::Start(size - MAX_READ_BYTES)).map_err(|e| e.to_string())?;
        skipped_prefix = true;
    }
    let mut bytes = Vec::new();
    match file.take(MAX_READ_BYTES).read_to_end(&mut bytes) {
        Ok(_) => {}
        Err(e) if e.kind() == ErrorKind::WouldBlock => {}
        Err(e) => return Err(format!("{}: {e}", canon.display())),
    }
    if bytes.iter().take(8192).any(|b| *b == 0) {
        return Err(format!("{} is a binary file", canon.display()));
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut all: Vec<&str> = text.lines().collect();
    if skipped_prefix && !all.is_empty() {
        all.remove(0); // probably a partial line
    }
    let total = all.len();
    let shown: Vec<&str> =
        if tail { all[total.saturating_sub(lines)..].to_vec() } else { all.into_iter().take(lines).collect() };
    let mut out = shown.join("\n");
    if total > shown.len() || skipped_prefix {
        let which = if tail { "last" } else { "first" };
        out.push_str(&format!("\n[showing the {which} {} lines of {}]", shown.len(), canon.display()));
    }
    if out.is_empty() {
        out = "(empty file)".into();
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    fn policy_for(dir: &Path) -> PathPolicy {
        PathPolicy { readable: vec![dir.to_path_buf()], ..PathPolicy::default() }
    }

    #[test]
    fn reads_with_limits() {
        let dir = tempfile::tempdir().unwrap();
        let p = policy_for(dir.path());
        let f = dir.path().join("log.txt");
        fs::write(&f, (1..=10).map(|i| format!("line {i}\n")).collect::<String>()).unwrap();
        let head = read_file(&f, 2, false, &p).unwrap();
        assert!(head.starts_with("line 1\nline 2\n[showing the first 2 lines"), "{head}");
        let tail = read_file(&f, 2, true, &p).unwrap();
        assert!(tail.starts_with("line 9\nline 10\n[showing the last 2"), "{tail}");
    }

    #[test]
    fn symlinks_cannot_reach_denied_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = policy_for(dir.path());
        let secret_dir = dir.path().join("secret");
        fs::create_dir(&secret_dir).unwrap();
        fs::write(secret_dir.join("data"), "top secret").unwrap();
        p.denied.push(secret_dir.clone());
        symlink(secret_dir.join("data"), dir.path().join("innocent")).unwrap();
        let err = read_file(&dir.path().join("innocent"), 5, false, &p).unwrap_err();
        assert!(err.contains("off limits"), "{err}");
        let err = read_file(Path::new("/etc/hostname"), 5, false, &p).unwrap_err();
        assert!(err.contains("outside the readable areas"), "{err}");
    }

    #[test]
    fn fifos_binaries_and_devices_are_refused_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = policy_for(dir.path());
        let fifo = dir.path().join("fifo");
        let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        // SAFETY: valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert!(read_file(&fifo, 5, false, &p).unwrap_err().contains("not a regular file"));
        fs::write(dir.path().join("bin"), [0u8, 1, 2, 3]).unwrap();
        assert!(read_file(&dir.path().join("bin"), 5, false, &p).unwrap_err().contains("binary"));
        p.readable.push("/dev".into());
        assert!(read_file(Path::new("/dev/zero"), 5, false, &p).unwrap_err().contains("not a regular file"));
    }

    #[test]
    fn lists_directories() {
        let dir = tempfile::tempdir().unwrap();
        let p = policy_for(dir.path());
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let out = list_directory(dir.path(), &p).unwrap();
        assert!(out.contains("a.txt  5 B") && out.contains("sub/"), "{out}");
        assert!(list_directory(&dir.path().join("a.txt"), &p).unwrap_err().contains("not a directory"));
    }
}
