//! Where build scripts run: on the host (cross stage) or inside the new root.

use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
            let mut c = Command::new("chroot");
            c.arg(root).arg("/usr/bin/env").arg("-i");
            for (k, v) in env {
                c.arg(format!("{k}={v}"));
            }
            c.arg("/bin/bash").arg("+h").arg("-c").arg(format!("cd {cwd}\n{body}"));
            c.env_clear();
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
