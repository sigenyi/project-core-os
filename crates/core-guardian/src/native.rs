//! Operations implemented directly instead of by spawning a program.
//!
//! Reading files and listing directories natively lets the Guardian enforce its path
//! policy on the *canonical* path (after following symlinks) and refuse device nodes,
//! FIFOs and binary files that would hang or flood the model.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};

use core_protocol::choice::Signal;

use crate::config::GuardianConfig;
use crate::plan::NativeOp;
use crate::policy::Policy;

const MAX_DIR_ENTRIES: usize = 200;
const MAX_READ_BYTES: u64 = 1024 * 1024;

pub fn execute(op: &NativeOp, config: &GuardianConfig) -> Result<String, String> {
    let policy = Policy::new(config);
    match op {
        NativeOp::ListDir { path } => list_dir(path, &policy),
        NativeOp::ReadFile { path, lines, tail } => read_file(path, *lines, *tail, &policy),
        NativeOp::SetBrightness { percent } => set_brightness(&config.native.sysfs, *percent),
        NativeOp::Signal { pid, signal } => send_signal(&config.native.procfs, *pid, *signal),
        NativeOp::RemoveSwapFile => remove_swap_file(&config.native.swapfile),
        NativeOp::FstabSwap { present } => fstab_swap(&config.native.fstab, &config.native.swapfile, *present),
    }
}

fn canonical_readable(path: &Path, policy: &Policy) -> Result<PathBuf, String> {
    let canon = fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
    policy.check_readable(&canon)?;
    Ok(canon)
}

fn list_dir(path: &Path, policy: &Policy) -> Result<String, String> {
    let canon = canonical_readable(path, policy)?;
    if !canon.is_dir() {
        return Err(format!("{} is not a directory", canon.display()));
    }
    let mut entries: Vec<(String, String)> = fs::read_dir(&canon)
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
    let mut text = format!("{}:\n", canon.display());
    text.push_str(&out.join("\n"));
    Ok(text)
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

fn read_file(path: &Path, lines: usize, tail: bool, policy: &Policy) -> Result<String, String> {
    let canon = canonical_readable(path, policy)?;
    let meta = fs::metadata(&canon).map_err(|e| format!("{}: {e}", canon.display()))?;
    let ft = meta.file_type();
    if ft.is_dir() {
        return Err(format!("{} is a directory; use list_directory", canon.display()));
    }
    if ft.is_block_device() || ft.is_char_device() || ft.is_fifo() || ft.is_socket() || !ft.is_file() {
        return Err(format!("{} is not a regular file", canon.display()));
    }
    let mut file = File::open(&canon).map_err(|e| format!("{}: {e}", canon.display()))?;
    // procfs/sysfs report size 0, so always read with a cap; for large files read the
    // relevant end only.
    let mut skipped_prefix = false;
    if tail && meta.len() > MAX_READ_BYTES {
        file.seek(SeekFrom::Start(meta.len() - MAX_READ_BYTES)).map_err(|e| e.to_string())?;
        skipped_prefix = true;
    }
    let mut bytes = Vec::new();
    file.take(MAX_READ_BYTES).read_to_end(&mut bytes).map_err(|e| format!("{}: {e}", canon.display()))?;
    if bytes.iter().take(8192).any(|b| *b == 0) {
        return Err(format!("{} is a binary file", canon.display()));
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut all: Vec<&str> = text.lines().collect();
    if skipped_prefix && !all.is_empty() {
        all.remove(0); // first line is probably partial
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

/// Pick a backlight, preferring firmware/platform interfaces as the kernel recommends.
fn backlight_device(sysfs: &Path) -> Result<PathBuf, String> {
    let dir = sysfs.join("class/backlight");
    let mut devices: Vec<(u8, PathBuf)> = fs::read_dir(&dir)
        .map_err(|_| "this machine has no controllable backlight".to_string())?
        .filter_map(|e| e.ok())
        .map(|e| {
            let kind = fs::read_to_string(e.path().join("type")).unwrap_or_default();
            let rank = match kind.trim() {
                "firmware" => 0,
                "platform" => 1,
                _ => 2,
            };
            (rank, e.path())
        })
        .collect();
    devices.sort();
    devices.into_iter().next().map(|(_, p)| p).ok_or_else(|| "this machine has no controllable backlight".into())
}

fn set_brightness(sysfs: &Path, percent: u32) -> Result<String, String> {
    let dev = backlight_device(sysfs)?;
    let max: u64 = fs::read_to_string(dev.join("max_brightness"))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .filter(|m| *m > 0)
        .ok_or("cannot read max_brightness")?;
    let value = (max * u64::from(percent.clamp(1, 100)) / 100).max(1);
    fs::write(dev.join("brightness"), value.to_string()).map_err(|e| format!("cannot set brightness: {e}"))?;
    Ok(format!(
        "brightness set to {value}/{max} ({percent}%) on {}",
        dev.file_name().unwrap_or_default().to_string_lossy()
    ))
}

fn send_signal(procfs: &Path, pid: u32, signal: Signal) -> Result<String, String> {
    let comm = fs::read_to_string(procfs.join(pid.to_string()).join("comm"))
        .map_err(|_| format!("no process with pid {pid}"))?;
    let sig = match signal {
        Signal::Term => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
        Signal::Hup => libc::SIGHUP,
        Signal::Int => libc::SIGINT,
    };
    let pid_t = libc::pid_t::try_from(pid).map_err(|_| "pid out of range".to_string())?;
    // SAFETY: kill(2) with a validated positive pid (>= 2) and a constant signal.
    if unsafe { libc::kill(pid_t, sig) } != 0 {
        return Err(format!("cannot signal {pid}: {}", std::io::Error::last_os_error()));
    }
    Ok(format!("sent SIG{} to {pid} ({})", signal.as_str().to_uppercase(), comm.trim()))
}

fn remove_swap_file(path: &Path) -> Result<String, String> {
    match fs::symlink_metadata(path) {
        Err(_) => Ok(format!("{} does not exist", path.display())),
        Ok(m) if !m.file_type().is_file() => {
            Err(format!("{} is not a regular file; refusing to remove it", path.display()))
        }
        Ok(_) => fs::remove_file(path)
            .map(|_| format!("removed {}", path.display()))
            .map_err(|e| format!("cannot remove {}: {e}", path.display())),
    }
}

fn fstab_swap(fstab: &Path, swapfile: &Path, present: bool) -> Result<String, String> {
    let current = fs::read_to_string(fstab).unwrap_or_default();
    let swap = swapfile.display().to_string();
    let is_entry = |line: &str| {
        let f: Vec<&str> = line.split_whitespace().collect();
        !line.trim_start().starts_with('#') && f.len() >= 3 && f[0] == swap && f[2] == "swap"
    };
    match (current.lines().any(is_entry), present) {
        (true, true) => return Ok(format!("{swap} already in {}", fstab.display())),
        (false, false) => return Ok(format!("{swap} was not in {}", fstab.display())),
        _ => {}
    }
    let mut lines: Vec<String> = current.lines().filter(|l| !is_entry(l)).map(String::from).collect();
    if present {
        lines.push(format!("{swap} none swap defaults 0 0"));
    }
    let mut body = lines.join("\n");
    body.push('\n');
    let tmp = fstab.with_extension("core-tmp");
    {
        let mut f = File::create(&tmp).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        f.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
    }
    let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644));
    fs::rename(&tmp, fstab).map_err(|e| format!("cannot update {}: {e}", fstab.display()))?;
    Ok(format!("{} {swap} in {}", if present { "added" } else { "removed" }, fstab.display()))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    fn config_for(dir: &Path) -> GuardianConfig {
        let mut c = GuardianConfig::default();
        c.paths.readable = vec![dir.to_path_buf()];
        c.native.sysfs = dir.join("sys");
        c.native.procfs = dir.join("proc");
        c.native.fstab = dir.join("fstab");
        c.native.swapfile = dir.join("swapfile");
        c
    }

    #[test]
    fn reads_files_with_limits() {
        let dir = tempfile::tempdir().unwrap();
        let c = config_for(dir.path());
        let f = dir.path().join("log.txt");
        fs::write(&f, (1..=10).map(|i| format!("line {i}\n")).collect::<String>()).unwrap();
        let head = execute(&NativeOp::ReadFile { path: f.clone(), lines: 2, tail: false }, &c).unwrap();
        assert!(head.starts_with("line 1\nline 2\n[showing the first 2 lines"), "{head}");
        let tail = execute(&NativeOp::ReadFile { path: f, lines: 2, tail: true }, &c).unwrap();
        assert!(tail.starts_with("line 9\nline 10\n[showing the last 2"), "{tail}");
    }

    #[test]
    fn symlinks_cannot_escape_the_policy() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = config_for(dir.path());
        let secret_dir = dir.path().join("secret");
        fs::create_dir(&secret_dir).unwrap();
        fs::write(secret_dir.join("data"), "top secret").unwrap();
        c.paths.denied.push(secret_dir.clone());
        symlink(secret_dir.join("data"), dir.path().join("innocent")).unwrap();
        let err =
            execute(&NativeOp::ReadFile { path: dir.path().join("innocent"), lines: 5, tail: false }, &c).unwrap_err();
        assert!(err.contains("off limits"), "{err}");
        // Outside the readable roots entirely.
        let err = execute(&NativeOp::ReadFile { path: "/etc/hostname".into(), lines: 5, tail: false }, &c).unwrap_err();
        assert!(err.contains("outside the readable areas"), "{err}");
    }

    #[test]
    fn refuses_binaries_and_devices() {
        let dir = tempfile::tempdir().unwrap();
        let c = config_for(dir.path());
        fs::write(dir.path().join("bin"), [0u8, 1, 2, 3]).unwrap();
        let err = execute(&NativeOp::ReadFile { path: dir.path().join("bin"), lines: 5, tail: false }, &c).unwrap_err();
        assert!(err.contains("binary"));
        let mut c2 = c.clone();
        c2.paths.readable.push("/dev".into());
        let err = execute(&NativeOp::ReadFile { path: "/dev/zero".into(), lines: 5, tail: false }, &c2).unwrap_err();
        assert!(err.contains("not a regular file"), "{err}");
    }

    #[test]
    fn lists_directories() {
        let dir = tempfile::tempdir().unwrap();
        let c = config_for(dir.path());
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let out = execute(&NativeOp::ListDir { path: dir.path().to_path_buf() }, &c).unwrap();
        assert!(out.contains("a.txt  5 B") && out.contains("sub/"), "{out}");
    }

    #[test]
    fn brightness() {
        let dir = tempfile::tempdir().unwrap();
        let c = config_for(dir.path());
        let dev = dir.path().join("sys/class/backlight/intel_backlight");
        fs::create_dir_all(&dev).unwrap();
        fs::write(dev.join("max_brightness"), "1000\n").unwrap();
        fs::write(dev.join("type"), "raw\n").unwrap();
        fs::write(dev.join("brightness"), "0").unwrap();
        execute(&NativeOp::SetBrightness { percent: 40 }, &c).unwrap();
        assert_eq!(fs::read_to_string(dev.join("brightness")).unwrap(), "400");
    }

    #[test]
    fn fstab_editing_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let c = config_for(dir.path());
        let fstab = dir.path().join("fstab");
        fs::write(&fstab, "# comment\nUUID=abc / ext4 defaults 0 1\n").unwrap();
        execute(&NativeOp::FstabSwap { present: true }, &c).unwrap();
        execute(&NativeOp::FstabSwap { present: true }, &c).unwrap();
        let text = fs::read_to_string(&fstab).unwrap();
        assert_eq!(text.matches(" swap ").count(), 1, "{text}");
        assert!(text.starts_with("# comment\nUUID=abc"));
        execute(&NativeOp::FstabSwap { present: false }, &c).unwrap();
        assert!(!fs::read_to_string(&fstab).unwrap().contains("swap"));
    }

    #[test]
    fn swap_file_removal_refuses_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let c = config_for(dir.path());
        fs::write(dir.path().join("precious"), "data").unwrap();
        symlink(dir.path().join("precious"), dir.path().join("swapfile")).unwrap();
        assert!(execute(&NativeOp::RemoveSwapFile, &c).is_err());
        assert!(dir.path().join("precious").exists());
    }

    #[test]
    fn signals_check_the_process_exists() {
        let dir = tempfile::tempdir().unwrap();
        let c = config_for(dir.path());
        let err = execute(&NativeOp::Signal { pid: 99999, signal: Signal::Term }, &c).unwrap_err();
        assert!(err.contains("no process"));
    }
}
