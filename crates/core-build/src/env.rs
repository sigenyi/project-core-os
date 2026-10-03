//! Where build scripts run: on the host (cross stage) or inside the new root.

use std::ffi::CString;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
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
/// usable by ordinary users), and nothing else, so the build machine's disk
/// device nodes are not in the build root. This isolates the file system only:
/// scripts still run as root on the build machine's kernel, processes and
/// network.
///
/// Every mount is made fresh. A mount that already exists at or below one of the
/// [`TARGETS`] was made by something else (by hand, by another core-build still
/// running, or by one that was killed), and nothing proves it is private, so
/// setup refuses to start rather than reuse it, and leaves it alone.
pub struct Mounts {
    mounted: Vec<PathBuf>,
}

/// The mount points under the build root that [`Mounts`] creates, in order, with
/// the file system type each must end up with.
pub const TARGETS: [(&str, &str); 6] = [
    ("dev", "tmpfs"),
    ("dev/pts", "devpts"),
    ("dev/shm", "tmpfs"),
    ("proc", "proc"),
    ("sys", "sysfs"),
    ("run", "tmpfs"),
];

/// Mount point and file system type of every mount, from /proc/self/mountinfo.
fn mount_table() -> Result<Vec<(PathBuf, String)>, String> {
    let file = File::open("/proc/self/mountinfo").map_err(|e| format!("/proc/self/mountinfo: {e}"))?;
    parse_mount_table(BufReader::new(file)).map_err(|e| format!("/proc/self/mountinfo: {e}"))
}

/// Parse mountinfo. A read error or a line not in the kernel's format is an
/// error: a mount table with entries silently missing could hide a mount.
fn parse_mount_table(reader: impl BufRead) -> Result<Vec<(PathBuf, String)>, String> {
    let mut table = Vec::new();
    for (n, line) in reader.lines().enumerate() {
        let line = line.map_err(|e| format!("reading line {}: {e}", n + 1))?;
        // id parent major:minor root mount-point options [optional...] - type source super-options
        let fields: Vec<&str> = line.split(' ').collect();
        let sep = fields.iter().skip(6).position(|f| *f == "-").map(|i| i + 6);
        match sep {
            Some(sep) if fields.len() >= sep + 4 && !fields[4].is_empty() && !fields[sep + 1].is_empty() => {
                table.push((PathBuf::from(unescape_mount_path(fields[4])), fields[sep + 1].to_string()));
            }
            _ => return Err(format!("line {} is not a mountinfo record: {line:?}", n + 1)),
        }
    }
    Ok(table)
}

/// mountinfo writes space, tab, newline and backslash in paths as octal escapes
/// (`\040`); without decoding them, a build root with a space in its path would
/// never match its mounts.
fn unescape_mount_path(field: &str) -> std::ffi::OsString {
    let b = field.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let octal = b
            .get(i + 1..i + 4)
            .filter(|d| d.iter().all(|c| (b'0'..=b'7').contains(c)))
            .map(|d| d.iter().fold(0u32, |n, c| n * 8 + u32::from(c - b'0')))
            .and_then(|n| u8::try_from(n).ok());
        match (b[i], octal) {
            (b'\\', Some(byte)) => {
                out.push(byte);
                i += 4;
            }
            (c, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    std::ffi::OsString::from_vec(out)
}

/// The mounts in `table` at or below one of the [`TARGETS`] under `root` (which
/// must be canonical).
pub fn existing_mounts(root: &Path, table: &[(PathBuf, String)]) -> Vec<(PathBuf, String)> {
    table.iter().filter(|(m, _)| TARGETS.iter().any(|(rel, _)| m.starts_with(root.join(rel)))).cloned().collect()
}

fn mounted_fs_type(path: &Path) -> Result<Option<String>, String> {
    let canon = fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(mount_table()?.into_iter().rev().find(|(m, _)| *m == canon).map(|(_, t)| t))
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

/// The directory `rel` under `root`, created if missing. Every component must be
/// a real directory: mount follows symlinks, so `dev` pointing elsewhere would
/// put the mount outside the build root, where the check for existing mounts
/// (which compares paths) never looked.
fn mount_target(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let mut path = root.to_path_buf();
    for component in Path::new(rel).components() {
        path.push(component);
        match fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_dir() => {}
            Ok(_) => {
                return Err(format!(
                    "{} is not a directory (a symlink or a file); refusing to mount on it",
                    path.display()
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&path).map_err(|e| format!("{}: {e}", path.display()))?
            }
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    Ok(path)
}

impl Mounts {
    pub fn setup(root: &Path) -> Result<Mounts, String> {
        let root = fs::canonicalize(root).map_err(|e| format!("{}: {e}", root.display()))?;
        let existing = existing_mounts(&root, &mount_table()?);
        if !existing.is_empty() {
            let list: Vec<String> = existing.iter().map(|(m, t)| format!("  {} ({t})", m.display())).collect();
            return Err(format!(
                "these are already mounted in the build root:\n{}\ncore-build makes its own private mounts and \
                 removes them when it exits, so these come from another core-build that is still running, one that \
                 was killed, or a manual mount. Check what they are and unmount them (deepest first), then run again.",
                list.join("\n")
            ));
        }
        let mut m = Mounts { mounted: Vec::new() };
        for (rel, fs_type) in TARGETS {
            let target = mount_target(&root, rel)?;
            let args: &[&str] = match rel {
                "dev" => &["-t", "tmpfs", "tmpfs", "-o", "mode=0755,nosuid"],
                "dev/pts" => &["-t", "devpts", "devpts", "-o", "newinstance,ptmxmode=0666,mode=0620,gid=5"],
                "dev/shm" => &["-t", "tmpfs", "tmpfs", "-o", "mode=1777,nosuid,nodev"],
                "proc" => &["-t", "proc", "proc"],
                "sys" => &["-t", "sysfs", "sysfs"],
                "run" => &["-t", "tmpfs", "tmpfs", "-o", "mode=0755,nosuid,nodev"],
                _ => unreachable!("every target has mount options"),
            };
            mount(args, &target)?;
            m.mounted.push(target.clone());
            let got = mounted_fs_type(&target)?;
            if got.as_deref() != Some(fs_type) {
                return Err(format!("{} should be a {fs_type} mount, but is {got:?}", target.display()));
            }
            if rel == "dev" {
                populate_dev(&target)?;
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn table(entries: &[(&str, &str)]) -> Vec<(PathBuf, String)> {
        entries.iter().map(|(m, t)| (PathBuf::from(m), t.to_string())).collect()
    }

    #[test]
    fn any_mount_at_or_below_a_target_is_refused() {
        let root = Path::new("/w/root");
        let host = [("/", "ext4"), ("/dev", "devtmpfs"), ("/dev/pts", "devpts"), ("/proc", "proc")];
        assert!(existing_mounts(root, &table(&host)).is_empty(), "the host's own mounts are not the build root's");
        let outside = [("/w/root/build", "tmpfs"), ("/w/root/devices", "tmpfs"), ("/w/root/sysroot", "ext4")];
        assert!(existing_mounts(root, &table(&outside)).is_empty(), "only the targets, matched by path component");
        // A leftover private /dev (or any tmpfs someone put there) is not trusted
        // just for being a tmpfs, and neither is a stale devpts, proc, sys or run,
        // nor something mounted underneath one.
        for stale in [
            ("/w/root/dev", "tmpfs"),
            ("/w/root/dev", "devtmpfs"),
            ("/w/root/dev/pts", "devpts"),
            ("/w/root/dev/shm", "tmpfs"),
            ("/w/root/proc", "proc"),
            ("/w/root/proc/sys/fs/binfmt_misc", "binfmt_misc"),
            ("/w/root/sys", "sysfs"),
            ("/w/root/run", "tmpfs"),
        ] {
            assert_eq!(existing_mounts(root, &table(&[stale])), table(&[stale]), "{stale:?}");
        }
    }

    #[test]
    fn mountinfo_is_parsed_strictly() {
        let good = "22 1 0:21 / /proc rw,nosuid shared:5 - proc proc rw\n\
                    30 22 0:27 / /w/my\\040root/dev rw - tmpfs tmpfs rw,mode=755\n\
                    31 30 0:28 / /w/x rw - fuse.sshfs host:/ rw\n";
        assert_eq!(
            parse_mount_table(good.as_bytes()).unwrap(),
            table(&[("/proc", "proc"), ("/w/my root/dev", "tmpfs"), ("/w/x", "fuse.sshfs")])
        );
        for bad in [
            "22 1 0:21 / /proc rw shared:5 proc proc rw\n", // no separator
            "22 1 0:21 / /proc rw - proc\n",                // fields missing after it
            "22 1 0:21 /proc - proc proc rw\n",             // fields missing before it
            "garbage\n",
        ] {
            let text = format!("{good}{bad}");
            let err = parse_mount_table(text.as_bytes()).unwrap_err();
            assert!(err.contains("line 4"), "{bad:?}: {err}");
        }
        // A read that fails part-way is an error, not a shorter table.
        struct FailsAfter(&'static [u8]);
        impl io::Read for FailsAfter {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.0.is_empty() {
                    return Err(io::Error::other("device went away"));
                }
                let n = buf.len().min(self.0.len());
                buf[..n].copy_from_slice(&self.0[..n]);
                self.0 = &self.0[n..];
                Ok(n)
            }
        }
        let err = parse_mount_table(BufReader::new(FailsAfter(b"22 1 0:21 / /proc rw - proc proc rw\n"))).unwrap_err();
        assert!(err.contains("device went away"), "{err}");
        // The running kernel's own table parses, and includes the root.
        assert!(mount_table().unwrap().iter().any(|(m, _)| m == Path::new("/")));
    }

    #[test]
    fn mount_targets_must_be_real_directories_inside_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let elsewhere = dir.path().join("elsewhere");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&elsewhere).unwrap();
        // Missing directories are created.
        assert_eq!(mount_target(&root, "proc").unwrap(), root.join("proc"));
        assert!(root.join("proc").is_dir());
        // dev pointing out of the root: refused, for dev and for anything under it,
        // and nothing is created at the other end.
        std::os::unix::fs::symlink(&elsewhere, root.join("dev")).unwrap();
        for rel in ["dev", "dev/pts", "dev/shm"] {
            let err = mount_target(&root, rel).unwrap_err();
            assert!(err.contains("not a directory"), "{rel}: {err}");
        }
        assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
        // Same for a symlink to a directory inside the root, and for a file.
        std::os::unix::fs::symlink("proc", root.join("sys")).unwrap();
        assert!(mount_target(&root, "sys").is_err());
        fs::write(root.join("run"), "").unwrap();
        assert!(mount_target(&root, "run").is_err());
    }

    #[test]
    fn mountinfo_escapes_are_decoded() {
        assert_eq!(unescape_mount_path("/w/my\\040root/dev"), "/w/my root/dev");
        assert_eq!(unescape_mount_path("/a\\011b\\012c\\134d"), "/a\tb\nc\\d");
        // Not an escape: left as it is.
        assert_eq!(unescape_mount_path("/a\\9x\\777"), "/a\\9x\\777");
        let root = Path::new("/w/my root");
        let t = vec![(PathBuf::from(unescape_mount_path("/w/my\\040root/proc")), "proc".to_string())];
        assert_eq!(existing_mounts(root, &t).len(), 1);
    }

    /// Mounts for real, so it needs root: `sudo cargo test -p core-build -- --ignored`.
    #[test]
    #[ignore]
    fn setup_makes_private_mounts_and_refuses_existing_ones() {
        let dir = tempfile::tempdir().unwrap();
        let root = &fs::canonicalize(dir.path()).unwrap();
        let mounted =
            |rel: &str| mount_table().unwrap().into_iter().rev().find(|(m, _)| *m == root.join(rel)).map(|(_, t)| t);
        let first = Mounts::setup(root).unwrap();
        for (rel, fs_type) in TARGETS {
            assert_eq!(mounted(rel).as_deref(), Some(fs_type), "{rel}");
        }
        use std::os::unix::fs::FileTypeExt;
        assert!(fs::metadata(root.join("dev/null")).unwrap().file_type().is_char_device());
        assert!(!root.join("dev/sda").exists() && !root.join("dev/vda").exists());
        // While those exist (another core-build running), a second setup must refuse
        // and must not unmount them.
        let err = Mounts::setup(root).err().expect("existing mounts must be refused");
        assert!(err.contains("already mounted"), "{err}");
        assert_eq!(mounted("proc").as_deref(), Some("proc"));
        drop(first);
        for (rel, _) in TARGETS {
            assert_eq!(mounted(rel), None, "{rel} left mounted");
        }
        // A tmpfs mounted at dev by someone else is refused too, and left in place.
        mount(&["-t", "tmpfs", "tmpfs"], &root.join("dev")).unwrap();
        assert!(Mounts::setup(root).is_err());
        assert_eq!(mounted("dev").as_deref(), Some("tmpfs"));
        Command::new("umount").arg(root.join("dev")).status().unwrap();
        drop(Mounts::setup(root).unwrap());
    }
}
