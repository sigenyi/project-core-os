//! Just enough ELF parsing to learn what a binary links against.
//!
//! For each 64-bit little-endian ELF file we read the dynamic section: `DT_NEEDED`
//! entries (libraries it requires) and `DT_SONAME` (the name it is known by, for
//! shared libraries). This lets packages declare their library dependencies
//! automatically and lets a repository be checked for missing libraries.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DynamicInfo {
    pub needed: Vec<String>,
    pub soname: Option<String>,
    pub interpreter: Option<String>,
}

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_INTERP: u32 = 3;
const DT_NULL: i64 = 0;
const DT_NEEDED: i64 = 1;
const DT_STRTAB: i64 = 5;
const DT_SONAME: i64 = 14;

struct Reader {
    file: File,
    len: u64,
}

impl Reader {
    fn bytes(&mut self, offset: u64, len: usize) -> Option<Vec<u8>> {
        if offset.checked_add(len as u64)? > self.len {
            return None;
        }
        self.file.seek(SeekFrom::Start(offset)).ok()?;
        let mut buf = vec![0u8; len];
        self.file.read_exact(&mut buf).ok()?;
        Some(buf)
    }

    fn cstr(&mut self, offset: u64) -> Option<String> {
        let mut out = Vec::new();
        let mut pos = offset;
        loop {
            let chunk = self.bytes(pos, 64.min((self.len - pos.min(self.len)) as usize).max(1))?;
            if let Some(end) = chunk.iter().position(|b| *b == 0) {
                out.extend_from_slice(&chunk[..end]);
                return String::from_utf8(out).ok();
            }
            out.extend_from_slice(&chunk);
            pos += chunk.len() as u64;
            if out.len() > 4096 {
                return None;
            }
        }
    }
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

/// Whether the file starts with the ELF magic.
pub fn is_elf(path: &Path) -> bool {
    let mut magic = [0u8; 4];
    File::open(path).and_then(|mut f| f.read_exact(&mut magic)).is_ok() && magic == *b"\x7fELF"
}

/// Dynamic linking information, or `None` for non-ELF, non-64-bit-LE or static files.
pub fn dynamic_info(path: &Path) -> Option<DynamicInfo> {
    let file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let mut r = Reader { file, len };
    let header = r.bytes(0, 64)?;
    if &header[0..4] != b"\x7fELF" || header[4] != 2 || header[5] != 1 {
        return None;
    }
    let phoff = u64_at(&header, 0x20);
    let phentsize = u16_at(&header, 0x36) as usize;
    let phnum = u16_at(&header, 0x38) as usize;
    if phentsize < 56 || phnum == 0 || phnum > 512 {
        return None;
    }
    let phdrs = r.bytes(phoff, phentsize * phnum)?;
    let mut loads = Vec::new();
    let mut dynamic = None;
    let mut info = DynamicInfo::default();
    for i in 0..phnum {
        let p = &phdrs[i * phentsize..(i + 1) * phentsize];
        let (ptype, offset, vaddr, filesz) = (u32_at(p, 0), u64_at(p, 8), u64_at(p, 16), u64_at(p, 32));
        match ptype {
            PT_LOAD => loads.push((vaddr, offset, filesz)),
            PT_DYNAMIC => dynamic = Some((offset, filesz)),
            PT_INTERP => info.interpreter = r.cstr(offset),
            _ => {}
        }
    }
    let (dyn_off, dyn_size) = dynamic?;
    let entries = r.bytes(dyn_off, dyn_size.min(64 * 1024) as usize)?;
    let mut needed_offsets = Vec::new();
    let mut soname_offset = None;
    let mut strtab_vaddr = None;
    for e in entries.chunks_exact(16) {
        let tag = u64_at(e, 0) as i64;
        let val = u64_at(e, 8);
        match tag {
            DT_NULL => break,
            DT_NEEDED => needed_offsets.push(val),
            DT_SONAME => soname_offset = Some(val),
            DT_STRTAB => strtab_vaddr = Some(val),
            _ => {}
        }
    }
    let strtab_vaddr = strtab_vaddr?;
    let strtab = loads
        .iter()
        .find(|(vaddr, _, size)| strtab_vaddr >= *vaddr && strtab_vaddr < vaddr + size)
        .map(|(vaddr, offset, _)| strtab_vaddr - vaddr + offset)?;
    for off in needed_offsets {
        if let Some(s) = r.cstr(strtab + off) {
            info.needed.push(s);
        }
    }
    info.soname = soname_offset.and_then(|off| r.cstr(strtab + off));
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_system_binaries() {
        // Any dynamically linked binary on the build host will do.
        let ls = ["/usr/bin/ls", "/bin/ls"].iter().map(Path::new).find(|p| p.exists()).unwrap();
        assert!(is_elf(ls));
        let info = dynamic_info(ls).expect("ls is dynamically linked");
        assert!(info.needed.iter().any(|n| n.starts_with("libc.so")), "{info:?}");
        assert!(info.interpreter.unwrap().contains("ld-linux"));
        assert_eq!(info.soname, None);
    }

    #[test]
    fn reads_library_sonames() {
        let libc = ["/usr/lib/x86_64-linux-gnu/libc.so.6", "/usr/lib/libc.so.6", "/lib64/libc.so.6"]
            .iter()
            .map(Path::new)
            .find(|p| p.exists())
            .unwrap();
        assert_eq!(dynamic_info(libc).unwrap().soname.as_deref(), Some("libc.so.6"));
    }

    #[test]
    fn non_elf_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("script");
        std::fs::write(&f, "#!/bin/sh\necho hi\n").unwrap();
        assert!(!is_elf(&f));
        assert_eq!(dynamic_info(&f), None);
    }
}
