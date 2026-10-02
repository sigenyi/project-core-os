use std::collections::HashSet;
use std::ffi::CString;

use super::Collector;
use crate::snapshot::{BlockDevice, Filesystem, Snapshot};
use crate::sysroot::Sysroot;

pub struct StorageCollector;

/// Kernel/virtual filesystems that say nothing about storage.
const PSEUDO_FS: &[&str] = &[
    "proc",
    "sysfs",
    "devtmpfs",
    "devpts",
    "cgroup",
    "cgroup2",
    "securityfs",
    "pstore",
    "bpf",
    "debugfs",
    "tracefs",
    "mqueue",
    "hugetlbfs",
    "configfs",
    "fusectl",
    "autofs",
    "binfmt_misc",
    "efivarfs",
    "ramfs",
    "nsfs",
    "rpc_pipefs",
    "selinuxfs",
    "squashfs",
    "fuse.portal",
    "fuse.gvfsd-fuse",
    "tmpfs",
    "overlay",
];

/// Filesystems where `statvfs` can block indefinitely if the server is gone.
const NETWORK_FS: &[&str] = &["nfs", "nfs4", "cifs", "smb3", "smbfs", "fuse.sshfs", "9p", "ceph", "glusterfs"];

impl Collector for StorageCollector {
    fn name(&self) -> &'static str {
        "storage"
    }

    fn collect(&self, root: &Sysroot, snap: &mut Snapshot) -> Result<(), String> {
        let mounts =
            root.read("/proc/mounts").or_else(|| root.read("/proc/self/mounts")).ok_or("cannot read /proc/mounts")?;
        snap.storage = parse_mounts(&mounts);
        if root.is_live() {
            for fs in &mut snap.storage {
                if NETWORK_FS.contains(&fs.fs_type.as_str()) {
                    continue;
                }
                if let Some((size, avail, pct)) = statvfs(&fs.mount) {
                    fs.size_mb = Some(size);
                    fs.avail_mb = Some(avail);
                    fs.used_pct = Some(pct);
                }
            }
        }
        snap.block_devices = block_devices(root);
        Ok(())
    }
}

pub(crate) fn parse_mounts(text: &str) -> Vec<Filesystem> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 4 {
            continue;
        }
        let (device, mount, fs_type, options) = (unescape(fields[0]), unescape(fields[1]), fields[2], fields[3]);
        let root_overlay = mount == "/" && matches!(fs_type, "overlay" | "tmpfs");
        if (PSEUDO_FS.contains(&fs_type) && !root_overlay)
            || ["/proc", "/sys", "/dev", "/run/credentials"].iter().any(|p| mount.starts_with(p))
        {
            continue;
        }
        if !seen.insert(mount.clone()) {
            continue;
        }
        out.push(Filesystem {
            mount,
            device,
            fs_type: fs_type.to_string(),
            read_only: options.split(',').any(|o| o == "ro"),
            ..Default::default()
        });
    }
    out
}

/// Decode the octal escapes used in /proc/mounts (`\040` for space).
fn unescape(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 4 <= bytes.len() && bytes[i + 1..i + 4].iter().all(|b| (b'0'..=b'7').contains(b)) {
            let v = (bytes[i + 1] - b'0') * 64 + (bytes[i + 2] - b'0') * 8 + (bytes[i + 3] - b'0');
            out.push(v);
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// (size MiB, available MiB, used %) as `df` computes it.
fn statvfs(path: &str) -> Option<(u64, u64, u8)> {
    let c = CString::new(path).ok()?;
    // SAFETY: `c` is a valid NUL-terminated string and `st` is a properly sized,
    // zero-initialised out-parameter.
    let st = unsafe {
        let mut st: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c.as_ptr(), &mut st) != 0 {
            return None;
        }
        st
    };
    let frsize = st.f_frsize as u64;
    let total = st.f_blocks as u64 * frsize;
    if total == 0 {
        return None;
    }
    let used = (st.f_blocks as u64 - st.f_bfree as u64) * frsize;
    let avail = st.f_bavail as u64 * frsize;
    let pct = (used * 100).div_ceil((used + avail).max(1)).min(100) as u8;
    Some((total >> 20, avail >> 20, pct))
}

fn block_devices(root: &Sysroot) -> Vec<BlockDevice> {
    root.list("/sys/block")
        .into_iter()
        .filter(|n| !["loop", "ram", "fd"].iter().any(|p| n.starts_with(p)))
        .map(|name| {
            let base = format!("/sys/block/{name}");
            BlockDevice {
                size_mb: (root.read_u64(format!("{base}/size")).unwrap_or(0) * 512) >> 20,
                removable: root.read_trim(format!("{base}/removable")).as_deref() == Some("1"),
                rotational: root.read_trim(format!("{base}/queue/rotational")).as_deref() == Some("1"),
                model: root.read_trim(format!("{base}/device/model")),
                name,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mounts_are_filtered_and_unescaped() {
        let text = "\
proc /proc proc rw,nosuid 0 0
/dev/nvme0n1p2 / ext4 rw,relatime 0 0
tmpfs /tmp tmpfs rw 0 0
/dev/nvme0n1p1 /boot vfat rw 0 0
/dev/sdb1 /run/media/core/My\\040Disk exfat ro 0 0
/dev/nvme0n1p2 / ext4 rw,relatime 0 0
cgroup2 /sys/fs/cgroup cgroup2 rw 0 0
";
        let fs = parse_mounts(text);
        let mounts: Vec<&str> = fs.iter().map(|f| f.mount.as_str()).collect();
        assert_eq!(mounts, ["/", "/boot", "/run/media/core/My Disk"]);
        assert!(fs[2].read_only);
        assert_eq!(fs[0].fs_type, "ext4");
    }

    #[test]
    fn overlay_root_is_kept() {
        let fs = parse_mounts("airootfs / overlay rw 0 0\n");
        assert_eq!(fs.len(), 1);
    }

    #[test]
    fn statvfs_root_works() {
        let (size, _avail, pct) = statvfs("/").expect("statvfs /");
        assert!(size > 0 && pct <= 100);
    }
}
