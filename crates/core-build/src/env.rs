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
///
/// `/dev` is private, as in systemd-nspawn: a tmpfs with the standard device
/// nodes and the permissions udev gives them on C.O.R.E. OS (so /dev/fuse is
/// usable by ordinary users), and nothing else; build scripts never see the
/// build machine's disks.
pub struct Mounts {
    mounted: Vec<PathBuf>,
}

/// Mount point and file system type of every mount, from /proc/self/mountinfo.
fn mount_table() -> Vec<(PathBuf, String)> {
    let Ok(file) = File::open("/proc/self/mountinfo") else { return Vec::new() };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| {
            let mount_point = l.split_whitespace().nth(4)?;
            let fs_type = l.split(" - ").nth(1)?.split_whitespace().next()?;
            Some((PathBuf::from(mount_point), fs_type.to_string()))
        })
        .collect()
}

fn mounted_fs_type(path: &Path) -> Option<String> {
    let canon = fs::canonicalize(path).ok()?;
    mount_table().into_iter().rev().find(|(m, _)| *m == canon).map(|(_, t)| t)
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

/// Character devices of the private /dev: name, major, minor, mode.
const DEVICES: &[(&str, u32, u32, u32)] = &[
    ("null", 1, 3, 0o666),
    ("zero", 1, 5, 0o666),
    ("full", 1, 7, 0o666),
    ("random", 1, 8, 0o666),
    ("urandom", 1, 9, 0o666),
    ("tty", 5, 0, 0o666),
    ("fuse", 10, 229, 0o666),
];

const DEV_LINKS: &[(&str, &str)] = &[
    ("fd", "/proc/self/fd"),
    ("stdin", "/proc/self/fd/0"),
    ("stdout", "/proc/self/fd/1"),
    ("stderr", "/proc/self/fd/2"),
    ("ptmx", "pts/ptmx"),
];

fn populate_dev(dev: &Path) -> Result<(), String> {
    for (name, major, minor, mode) in DEVICES {
        let path = CString::new(dev.join(name).as_os_str().as_bytes()).map_err(|e| e.to_string())?;
        // SAFETY: plain system calls on a valid C string.
        check(unsafe { libc::mknod(path.as_ptr(), libc::S_IFCHR | mode, libc::makedev(*major, *minor)) })
            .map_err(|e| format!("mknod {}: {e}", dev.join(name).display()))?;
        // mknod applies the umask; set the mode explicitly.
        check(unsafe { libc::chmod(path.as_ptr(), *mode) }).map_err(|e| e.to_string())?;
    }
    for (name, target) in DEV_LINKS {
        std::os::unix::fs::symlink(target, dev.join(name)).map_err(|e| format!("{}: {e}", dev.join(name).display()))?;
    }
    for dir in ["pts", "shm"] {
        fs::create_dir_all(dev.join(dir)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

impl Mounts {
    pub fn setup(root: &Path) -> Result<Mounts, String> {
        let mut m = Mounts { mounted: Vec::new() };
        let dev = root.join("dev");
        fs::create_dir_all(&dev).map_err(|e| format!("{}: {e}", dev.display()))?;
        match mounted_fs_type(&dev).as_deref() {
            None => {
                mount(&["-t", "tmpfs", "tmpfs", "-o", "mode=0755,nosuid"], &dev)?;
                m.mounted.push(dev.clone());
                populate_dev(&dev)?;
            }
            Some("tmpfs") => {}
            Some(other) => {
                return Err(format!(
                    "{} is a {other} mount (an old bind of the host's /dev?); unmount it so a private /dev can be made",
                    dev.display()
                ));
            }
        }
        let table: [(&str, &[&str]); 5] = [
            ("dev/pts", &["-t", "devpts", "devpts", "-o", "newinstance,ptmxmode=0666,mode=0620,gid=5"]),
            ("dev/shm", &["-t", "tmpfs", "tmpfs", "-o", "mode=1777,nosuid,nodev"]),
            ("proc", &["-t", "proc", "proc"]),
            ("sys", &["-t", "sysfs", "sysfs"]),
            ("run", &["-t", "tmpfs", "tmpfs", "-o", "mode=0755,nosuid,nodev"]),
        ];
        for (rel, args) in table {
            let target = root.join(rel);
            fs::create_dir_all(&target).map_err(|e| format!("{}: {e}", target.display()))?;
            if mounted_fs_type(&target).is_some() {
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
