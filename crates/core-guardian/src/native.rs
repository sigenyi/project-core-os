//! Operations implemented directly instead of by spawning a program.
//!
//! None of these takes a path chosen by the model: files the user asks about are read
//! by the unprivileged agent, never by root.

use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use core_protocol::choice::Signal;

use crate::config::GuardianConfig;
use crate::plan::NativeOp;

const MAX_READ_BYTES: u64 = 64 * 1024;

pub fn execute(op: &NativeOp, config: &GuardianConfig) -> Result<String, String> {
    match op {
        NativeOp::ReadFixedFile { path, lines } => read_fixed_file(Path::new(path), *lines),
        NativeOp::SetBrightness { percent } => set_brightness(&config.native.sysfs, *percent),
        NativeOp::Signal { pid, signal } => send_signal(&config.native.procfs, *pid, *signal),
        NativeOp::RemoveSwapFile => remove_swap_file(&config.native.swapfile),
        NativeOp::FstabSwap { present } => fstab_swap(&config.native.fstab, &config.native.swapfile, *present),
    }
}

/// Read the first `lines` lines of a fixed system file, never blocking.
fn read_fixed_file(path: &Path, lines: usize) -> Result<String, String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if !file.metadata().map(|m| m.is_file()).unwrap_or(false) {
        return Err(format!("{} is not a regular file", path.display()));
    }
    let mut bytes = Vec::new();
    match file.take(MAX_READ_BYTES).read_to_end(&mut bytes) {
        Ok(_) => {}
        Err(e) if e.kind() == ErrorKind::WouldBlock => {}
        Err(e) => return Err(format!("{}: {e}", path.display())),
    }
    let text = String::from_utf8_lossy(&bytes);
    Ok(text.lines().take(lines).collect::<Vec<_>>().join("\n"))
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
    // kill(2) on a thread id signals the whole thread group, so resolve it first.
    let tgid = fs::read_to_string(procfs.join(pid.to_string()).join("status"))
        .ok()
        .and_then(|s| s.lines().find_map(|l| l.strip_prefix("Tgid:").and_then(|v| v.trim().parse::<u32>().ok())))
        .unwrap_or(pid);
    if tgid == std::process::id() || tgid == 1 {
        return Err("refusing to signal the Guardian itself or init".into());
    }
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
        c.native.sysfs = dir.join("sys");
        c.native.procfs = dir.join("proc");
        c.native.fstab = dir.join("fstab");
        c.native.swapfile = dir.join("swapfile");
        c
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
    fn fixed_reads_do_not_block_on_fifos() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("fifo");
        let c_path = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        // SAFETY: valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        let err = read_fixed_file(&fifo, 5).unwrap_err();
        assert!(err.contains("not a regular file"), "{err}");
        assert!(read_fixed_file(Path::new("/etc/hostname"), 1).is_ok() || !Path::new("/etc/hostname").exists());
    }

    #[test]
    fn threads_of_the_guardian_are_protected() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let worker = std::thread::spawn(move || {
            // SAFETY: gettid has no preconditions.
            tx.send(unsafe { libc::gettid() } as u32).unwrap();
            let _ = stop_rx.recv();
        });
        let tid = rx.recv().unwrap();
        let err = send_signal(Path::new("/proc"), tid, Signal::Term).unwrap_err();
        assert!(err.contains("Guardian itself"), "{err}");
        stop_tx.send(()).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn signals_check_the_process_exists() {
        let dir = tempfile::tempdir().unwrap();
        let c = config_for(dir.path());
        let err = execute(&NativeOp::Signal { pid: 99999, signal: Signal::Term }, &c).unwrap_err();
        assert!(err.contains("no process"));
    }
}
