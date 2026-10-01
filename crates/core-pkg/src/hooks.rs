//! Transaction hooks: commands run once after a transaction touched matching paths.
//!
//! Packages ship hooks in `usr/share/cpkg/hooks/*.toml`, for example glibc runs
//! `ldconfig` when libraries change and systemd creates users from `sysusers.d`.
//!
//! ```toml
//! description = "Updating the shared library cache"
//! paths = ["usr/lib/*.so*", "etc/ld.so.conf.d/**"]
//! exec = ["/usr/bin/ldconfig"]
//! ```
//!
//! When installing into another root, hooks run chrooted into it.

use std::fs;
use std::path::Path;
use std::process::Command;

use serde::Deserialize;

pub const HOOK_DIR: &str = "usr/share/cpkg/hooks";

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hook {
    #[serde(skip)]
    pub name: String,
    pub description: String,
    pub paths: Vec<String>,
    pub exec: Vec<String>,
    /// Run order among hooks (lower first).
    #[serde(default = "default_order")]
    pub order: i32,
}

fn default_order() -> i32 {
    50
}

/// Glob matching on `/`-separated paths: `*` stays within a component, `**` spans
/// components, `?` matches one character.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    fn rec(p: &[u8], s: &[u8]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some(b'*') if p.get(1) == Some(&b'*') => {
                let rest = &p[2..];
                (0..=s.len()).any(|i| rec(rest, &s[i..]))
            }
            Some(b'*') => {
                let rest = &p[1..];
                for i in 0..=s.len() {
                    if rec(rest, &s[i..]) {
                        return true;
                    }
                    if i < s.len() && s[i] == b'/' {
                        break;
                    }
                }
                false
            }
            Some(b'?') => !s.is_empty() && s[0] != b'/' && rec(&p[1..], &s[1..]),
            Some(c) => s.first() == Some(c) && rec(&p[1..], &s[1..]),
        }
    }
    rec(pattern.as_bytes(), path.as_bytes())
}

pub fn load(root: &Path) -> Vec<Hook> {
    let mut hooks = Vec::new();
    if let Ok(rd) = fs::read_dir(root.join(HOOK_DIR)) {
        for e in rd.filter_map(|e| e.ok()) {
            let path = e.path();
            if path.extension().is_none_or(|x| x != "toml") {
                continue;
            }
            match fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|t| toml::from_str::<Hook>(&t).map_err(|e| e.to_string()))
            {
                Ok(mut h) => {
                    h.name = path.file_stem().unwrap().to_string_lossy().into_owned();
                    hooks.push(h);
                }
                Err(err) => log::warn!("ignoring hook {}: {err}", path.display()),
            }
        }
    }
    hooks.sort_by(|a, b| a.order.cmp(&b.order).then(a.name.cmp(&b.name)));
    hooks
}

/// Hooks whose patterns match any of the changed paths.
pub fn triggered<'a>(hooks: &'a [Hook], changed: &[String]) -> Vec<&'a Hook> {
    hooks.iter().filter(|h| changed.iter().any(|c| h.paths.iter().any(|p| glob_match(p, c)))).collect()
}

/// Run a hook in `root`. Missing programs are skipped (they may arrive later in a
/// bootstrap); failures are reported but do not undo the transaction.
pub fn run(hook: &Hook, root: &Path) -> Result<(), String> {
    let Some(program) = hook.exec.first() else { return Ok(()) };
    let rel = program.trim_start_matches('/');
    if !root.join(rel).exists() {
        log::info!("skipping hook {}: {program} is not installed", hook.name);
        return Ok(());
    }
    let status = if root == Path::new("/") {
        Command::new(program).args(&hook.exec[1..]).status()
    } else {
        Command::new("chroot").arg(root).args(&hook.exec).env_clear().env("PATH", "/usr/bin").status()
    }
    .map_err(|e| format!("hook {}: {e}", hook.name))?;
    if status.success() { Ok(()) } else { Err(format!("hook {} ({}) failed: {status}", hook.name, hook.description)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match("usr/lib/*.so*", "usr/lib/libc.so.6"));
        assert!(!glob_match("usr/lib/*.so*", "usr/lib/x/libc.so.6"));
        assert!(glob_match("usr/lib/sysusers.d/*.conf", "usr/lib/sysusers.d/core.conf"));
        assert!(glob_match("etc/ld.so.conf.d/**", "etc/ld.so.conf.d/a/b.conf"));
        assert!(glob_match("usr/share/man/**", "usr/share/man/man1/ls.1"));
        assert!(glob_match("usr/bin/?s", "usr/bin/ls"));
        assert!(!glob_match("usr/bin/?s", "usr/bin/lss"));
    }

    #[test]
    fn loading_and_triggering() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = dir.path().join(HOOK_DIR);
        fs::create_dir_all(&hooks).unwrap();
        fs::write(
            hooks.join("ldconfig.toml"),
            "description = \"libs\"\npaths = [\"usr/lib/*.so*\"]\nexec = [\"/usr/bin/ldconfig\"]\norder = 10\n",
        )
        .unwrap();
        fs::write(hooks.join("broken.toml"), "nonsense").unwrap();
        let loaded = load(dir.path());
        assert_eq!(loaded.len(), 1);
        assert_eq!(triggered(&loaded, &["usr/lib/libz.so.1".into()]).len(), 1);
        assert!(triggered(&loaded, &["usr/bin/ls".into()]).is_empty());
        // Not installed in this root: skipped, not an error.
        assert!(run(&loaded[0], dir.path()).is_ok());
    }
}
