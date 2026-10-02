//! Where build scripts run: on the host (cross stage) or inside the new root.

use std::ffi::CString;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn check(ret: libc::c_int) -> io::Result<()> {
    if ret == -1 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

/// A command that runs `program` (a path inside `root`) with `root` as its
/// filesystem root.
///
/// Unlike chroot(2), the child gets its own mount namespace whose root *is* the
/// build root (bind-mounted onto itself, then pivot_root), as bubblewrap and
/// systemd-nspawn do. Inside a plain chroot the kernel refuses new user
/// namespaces and private mounts, which disables glibc's container-based tests.
/// The virtual filesystems mounted under `root` (see [`Mounts`]) come along with
/// the recursive bind mount.
pub fn command_in_root(root: &Path, program: &str) -> Result<Command, String> {
    let root_c = CString::new(root.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let slash = CString::new("/").unwrap();
    let dot = CString::new(".").unwrap();
    let mut cmd = Command::new(program);
    // SAFETY: the closure runs in the forked child before exec and only makes
    // async-signal-safe system calls on strings allocated before the fork.
    unsafe {
        cmd.pre_exec(move || {
            check(libc::unshare(libc::CLONE_NEWNS))?;
            // Keep our mounts from propagating back to the host.
            check(libc::mount(
                std::ptr::null(),
                slash.as_ptr(),
                std::ptr::null(),
                libc::MS_REC | libc::MS_PRIVATE,
                std::ptr::null(),
            ))?;
            check(libc::mount(
                root_c.as_ptr(),
                root_c.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND | libc::MS_REC,
                std::ptr::null(),
            ))?;
            check(libc::chdir(root_c.as_ptr()))?;
            // pivot_root(".", ".") stacks the old root on top of the new one;
            // detaching "." then leaves only the build root.
            check(libc::syscall(libc::SYS_pivot_root, dot.as_ptr(), dot.as_ptr()) as libc::c_int)?;
            check(libc::umount2(dot.as_ptr(), libc::MNT_DETACH))?;
            check(libc::chdir(slash.as_ptr()))?;
            Ok(())
        });
    }
    Ok(cmd)
}

/// The kernel's virtual filesystems mounted inside the build root, unmounted on drop.
pub struct Mounts {
    mounted: Vec<PathBuf>,
}

fn is_mountpoint(path: &Path) -> bool {
    let Ok(canon) = fs::canonicalize(path) else { return false };
    let Ok(file) = File::open("/proc/self/mountinfo") else { return false };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .any(|l| l.split_whitespace().nth(4).is_some_and(|m| Path::new(m) == canon))
}

fn mount(args: &[&str], target: &Path) -> Result<(), String> {
    let out = Command::new("mount").args(args).arg(target).output().map_err(|e| format!("mount: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "mount {} {}: {}",
            args.join(" "),
            target.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

impl Mounts {
    pub fn setup(root: &Path) -> Result<Mounts, String> {
        let mut m = Mounts { mounted: Vec::new() };
        let table: [(&str, &[&str]); 5] = [
            ("dev", &["--bind", "/dev"]),
            ("dev/pts", &["-t", "devpts", "devpts", "-o", "gid=5,mode=0620"]),
            ("proc", &["-t", "proc", "proc"]),
            ("sys", &["-t", "sysfs", "sysfs"]),
            ("run", &["-t", "tmpfs", "tmpfs"]),
        ];
        for (rel, args) in table {
            let target = root.join(rel);
            fs::create_dir_all(&target).map_err(|e| format!("{}: {e}", target.display()))?;
            if is_mountpoint(&target) {
                continue;
            }
            mount(args, &target)?;
            m.mounted.push(target);
        }
        Ok(m)
    }
}

impl Drop for Mounts {
    fn drop(&mut self) {
        for target in self.mounted.iter().rev() {
            let _ = Command::new("umount").arg("-l").arg(target).status();
        }
    }
}

pub enum Place<'a> {
    /// On the build host, in `cwd`.
    Host { cwd: &'a Path },
    /// Inside `root`, in `cwd` (a path inside the root).
    Chroot { root: &'a Path, cwd: &'a str },
}

/// Run a build script with exactly `env` (nothing inherited), logging to `log`.
pub fn run_script(place: Place, script: &str, env: &[(String, String)], log: &Path) -> Result<(), String> {
    if let Some(dir) = log.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let out = File::create(log).map_err(|e| format!("{}: {e}", log.display()))?;
    let err = out.try_clone().map_err(|e| e.to_string())?;
    let body = format!("set -e -o pipefail\n{script}");
    let mut cmd = match place {
        Place::Host { cwd } => {
            let mut c = Command::new("/bin/bash");
            c.arg("+h").arg("-c").arg(&body).current_dir(cwd).env_clear().envs(env.iter().map(|(k, v)| (k, v)));
            c
        }
        Place::Chroot { root, cwd } => {
            let mut c = command_in_root(root, "/usr/bin/bash")?;
            c.arg("+h").arg("-c").arg(format!("cd {cwd}\n{body}"));
            c.env_clear().envs(env.iter().map(|(k, v)| (k, v)));
            c
        }
    };
    let status = cmd
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .status()
        .map_err(|e| format!("cannot start build script: {e}"))?;
    if status.success() {
        return Ok(());
    }
    let text = fs::read_to_string(log).unwrap_or_default();
    let tail: Vec<&str> = text.lines().rev().take(40).collect::<Vec<_>>().into_iter().rev().collect();
    Err(format!("build script failed ({status}); last lines of {}:\n{}", log.display(), tail.join("\n")))
}
