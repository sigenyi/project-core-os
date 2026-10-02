//! Repositories: a directory of `.cpk` files plus a signed `index.json`.
//!
//! The index lists every package with its checksum, dependencies and what it
//! provides. It is signed with an ed25519 key; clients trust only indexes signed by a
//! key in `/etc/cpkg/keys/`, and every downloaded package must match the checksum
//! in the verified index. A repository can be checked for *closure*: every
//! dependency, including every shared library any binary links against, must be
//! satisfiable from inside the repository.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::archive::{read_metadata, sha256_file};
use crate::manifest::{AiInfo, Depends, Manifest, Provides};
use crate::version::Version;

pub const INDEX_FILE: &str = "index.json";
pub const SIGNATURE_FILE: &str = "index.json.sig";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexEntry {
    pub name: String,
    pub version: String,
    pub release: u32,
    pub arch: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub file: String,
    pub sha256: String,
    pub size: u64,
    pub installed_size: u64,
    pub depends: Depends,
    pub provides: Provides,
    #[serde(default, skip_serializing_if = "AiInfo::is_empty")]
    pub ai: AiInfo,
}

impl IndexEntry {
    pub fn from_manifest(m: &Manifest, file: String, sha256: String, size: u64) -> Self {
        IndexEntry {
            name: m.package.name.clone(),
            version: m.package.version.clone(),
            release: m.package.release,
            arch: m.package.arch.clone(),
            summary: m.package.summary.clone(),
            description: m.package.description.clone(),
            file,
            sha256,
            size,
            installed_size: m.package.installed_size,
            depends: m.depends.clone(),
            provides: Provides { config: Vec::new(), ..m.provides.clone() },
            ai: m.ai.clone(),
        }
    }

    pub fn version(&self) -> Version {
        Version::new(&self.version, self.release)
    }

    pub fn id(&self) -> String {
        format!("{}-{}-{}", self.name, self.version, self.release)
    }

    /// Whether this package satisfies a dependency on `name` (a package or provided name).
    pub fn satisfies(&self, name: &str) -> bool {
        self.name == name || self.provides.names.iter().any(|n| n == name)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Index {
    pub format: u32,
    pub packages: Vec<IndexEntry>,
}

impl Index {
    /// Scan a directory of packages (newest version of each name wins).
    pub fn scan(dir: &Path) -> Result<Index, String> {
        let mut best: BTreeMap<String, IndexEntry> = BTreeMap::new();
        let mut paths: Vec<PathBuf> = fs::read_dir(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "cpk"))
            .collect();
        paths.sort();
        for path in paths {
            let opened = read_metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let size = fs::metadata(&path).map_err(|e| e.to_string())?.len();
            let file = path.file_name().unwrap().to_string_lossy().into_owned();
            let entry =
                IndexEntry::from_manifest(&opened.manifest, file, sha256_file(&path).map_err(|e| e.to_string())?, size);
            match best.get(&entry.name) {
                Some(existing) if existing.version() >= entry.version() => {}
                _ => {
                    best.insert(entry.name.clone(), entry);
                }
            }
        }
        Ok(Index { format: 1, packages: best.into_values().collect() })
    }

    pub fn get(&self, name: &str) -> Option<&IndexEntry> {
        self.packages.iter().find(|p| p.name == name)
    }

    /// The best package for a dependency name: exact name first, else a provider.
    pub fn provider(&self, name: &str) -> Option<&IndexEntry> {
        self.get(name).or_else(|| self.packages.iter().filter(|p| p.satisfies(name)).max_by_key(|p| p.version()))
    }

    /// The package providing a shared library soname.
    pub fn library_provider(&self, soname: &str) -> Option<&IndexEntry> {
        self.packages.iter().find(|p| p.provides.libraries.iter().any(|l| l == soname))
    }

    /// Every unsatisfiable dependency in the repository.
    pub fn closure_problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        for p in &self.packages {
            for d in &p.depends.packages {
                if self.provider(d).is_none() {
                    problems.push(format!("{}: depends on {d}, which no package provides", p.name));
                }
            }
            for lib in &p.depends.libraries {
                if self.library_provider(lib).is_none() {
                    problems.push(format!("{}: needs library {lib}, which no package provides", p.name));
                }
            }
        }
        problems
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("index serialises")
    }

    pub fn from_json(text: &str) -> Result<Index, String> {
        serde_json::from_str(text).map_err(|e| format!("invalid repository index: {e}"))
    }

    /// Free-text search over names, summaries, descriptions, programs and keywords.
    pub fn search(&self, query: &str) -> Vec<&IndexEntry> {
        let words: Vec<String> = query.to_lowercase().split_whitespace().map(String::from).collect();
        let mut scored: Vec<(usize, &IndexEntry)> = self
            .packages
            .iter()
            .filter_map(|p| {
                let haystack = format!(
                    "{} {} {} {} {}",
                    p.name,
                    p.summary,
                    p.description,
                    p.provides.binaries.join(" "),
                    p.ai.keywords.join(" ")
                )
                .to_lowercase();
                let hits = words.iter().filter(|w| haystack.contains(w.as_str())).count();
                let name_bonus = usize::from(words.contains(&p.name)) * 10;
                (hits == words.len() && hits > 0).then_some((hits + name_bonus, p))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.name.cmp(&b.1.name)));
        scored.into_iter().map(|(_, p)| p).collect()
    }
}

/// Write `index.json` and its signature into a repository directory.
pub fn write_index(dir: &Path, index: &Index, key: &SigningKey) -> Result<(), String> {
    let body = index.to_json();
    let sig = key.sign(body.as_bytes());
    fs::write(dir.join(INDEX_FILE), &body).map_err(|e| e.to_string())?;
    fs::write(dir.join(SIGNATURE_FILE), hex::encode(sig.to_bytes())).map_err(|e| e.to_string())?;
    Ok(())
}

/// Verify an index body against a signature with any of the trusted keys.
pub fn verify_index(body: &[u8], signature_hex: &str, trusted: &[VerifyingKey]) -> Result<(), String> {
    let bytes = hex::decode(signature_hex.trim()).map_err(|_| "malformed index signature".to_string())?;
    let bytes: [u8; 64] = bytes.try_into().map_err(|_| "malformed index signature".to_string())?;
    let sig = Signature::from_bytes(&bytes);
    if trusted.iter().any(|k| k.verify(body, &sig).is_ok()) {
        Ok(())
    } else if trusted.is_empty() {
        Err("no trusted repository keys are installed (/etc/cpkg/keys)".into())
    } else {
        Err("repository index signature is not valid for any trusted key".into())
    }
}

// ---- keys -----------------------------------------------------------------------

fn random_seed() -> Result<[u8; 32], String> {
    let mut seed = [0u8; 32];
    // SAFETY: getrandom(2) writes at most 32 bytes into the buffer.
    let n = unsafe { libc::getrandom(seed.as_mut_ptr().cast(), seed.len(), 0) };
    if n != 32 {
        return Err("no entropy available".into());
    }
    Ok(seed)
}

/// Create `<name>.key` (secret, 0600) and `<name>.pub` in `dir`.
pub fn generate_key(dir: &Path, name: &str) -> Result<(PathBuf, PathBuf), String> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let key = SigningKey::from_bytes(&random_seed()?);
    let secret = dir.join(format!("{name}.key"));
    let public = dir.join(format!("{name}.pub"));
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&secret)
        .map_err(|e| format!("{}: {e}", secret.display()))?;
    std::io::Write::write_all(&mut f, hex::encode(key.to_bytes()).as_bytes()).map_err(|e| e.to_string())?;
    fs::write(&public, hex::encode(key.verifying_key().to_bytes())).map_err(|e| e.to_string())?;
    Ok((secret, public))
}

pub fn load_signing_key(path: &Path) -> Result<SigningKey, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let bytes: [u8; 32] = hex::decode(text.trim())
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| format!("{}: not an ed25519 secret key", path.display()))?;
    Ok(SigningKey::from_bytes(&bytes))
}

pub fn parse_public_key(text: &str) -> Result<VerifyingKey, String> {
    let bytes: [u8; 32] =
        hex::decode(text.trim()).ok().and_then(|b| b.try_into().ok()).ok_or("not an ed25519 public key")?;
    VerifyingKey::from_bytes(&bytes).map_err(|e| e.to_string())
}

/// All `*.pub` keys in a directory.
pub fn load_trusted_keys(dir: &Path) -> Vec<VerifyingKey> {
    let mut keys = Vec::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.filter_map(|e| e.ok()) {
            if e.path().extension().is_some_and(|x| x == "pub") {
                match fs::read_to_string(e.path()).map_err(|e| e.to_string()).and_then(|t| parse_public_key(&t)) {
                    Ok(k) => keys.push(k),
                    Err(err) => log::warn!("ignoring key {}: {err}", e.path().display()),
                }
            }
        }
    }
    keys
}

/// A repository whose index has been verified.
pub struct Repository {
    pub location: String,
    pub index: Index,
    local_dir: Option<PathBuf>,
}

impl Repository {
    /// Open a repository (local path or `file://`/`http(s)://` URL) and verify its index.
    pub fn open(location: &str, trusted: &[VerifyingKey], verify: bool) -> Result<Repository, String> {
        let (body, sig, local_dir) = if let Some(path) = location
            .strip_prefix("file://")
            .or_else(|| (!location.starts_with("http://") && !location.starts_with("https://")).then_some(location))
        {
            let dir = PathBuf::from(path);
            let body =
                fs::read(dir.join(INDEX_FILE)).map_err(|e| format!("{}: {e}", dir.join(INDEX_FILE).display()))?;
            let sig = fs::read_to_string(dir.join(SIGNATURE_FILE)).unwrap_or_default();
            (body, sig, Some(dir))
        } else {
            let base = location.trim_end_matches('/');
            let body = http_get(&format!("{base}/{INDEX_FILE}"))?;
            let sig = String::from_utf8(http_get(&format!("{base}/{SIGNATURE_FILE}")).unwrap_or_default())
                .unwrap_or_default();
            (body, sig, None)
        };
        if verify {
            verify_index(&body, &sig, trusted).map_err(|e| format!("{location}: {e}"))?;
        }
        let index = Index::from_json(&String::from_utf8_lossy(&body))?;
        Ok(Repository { location: location.to_string(), index, local_dir })
    }

    /// Path to a verified local copy of a package file.
    pub fn fetch(&self, entry: &IndexEntry, cache: &Path) -> Result<PathBuf, String> {
        if entry.file.contains('/') || entry.file.starts_with('.') {
            return Err(format!("suspicious package file name {:?}", entry.file));
        }
        let path = match &self.local_dir {
            Some(dir) => dir.join(&entry.file),
            None => {
                fs::create_dir_all(cache).map_err(|e| e.to_string())?;
                let dest = cache.join(&entry.file);
                if !dest.exists() || sha256_file(&dest).ok().as_deref() != Some(entry.sha256.as_str()) {
                    let data = http_get(&format!("{}/{}", self.location.trim_end_matches('/'), entry.file))?;
                    fs::write(&dest, data).map_err(|e| e.to_string())?;
                }
                dest
            }
        };
        let actual = sha256_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if actual != entry.sha256 {
            return Err(format!("{}: checksum mismatch (index {}, file {actual})", entry.file, entry.sha256));
        }
        Ok(path)
    }
}

fn http_get(url: &str) -> Result<Vec<u8>, String> {
    let mut resp = ureq::get(url).call().map_err(|e| format!("{url}: {e}"))?;
    resp.body_mut().with_config().limit(4 << 30).read_to_vec().map_err(|e| format!("{url}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::create_package;
    use crate::archive::tests::{manifest, stage};

    fn repo_with(dir: &Path, pkgs: &[(&str, &str)]) {
        for (name, version) in pkgs {
            let dest = dir.join(format!("stage-{name}-{version}"));
            stage(&dest);
            // Different names must not collide on paths for later install tests.
            let mut m = manifest(name, version);
            m.package.summary = format!("{name} is a friendly greeter");
            m.ai.keywords = vec!["greeting".into()];
            create_package(&dest, m, dir).unwrap();
            fs::remove_dir_all(&dest).unwrap();
        }
    }

    #[test]
    fn index_keeps_newest_and_signs() {
        let dir = tempfile::tempdir().unwrap();
        repo_with(dir.path(), &[("hello", "1.0"), ("hello", "1.2"), ("hello", "1.10")]);
        let index = Index::scan(dir.path()).unwrap();
        assert_eq!(index.packages.len(), 1);
        assert_eq!(index.packages[0].version, "1.10");

        let keys = dir.path().join("keys");
        let (secret, public) = generate_key(&keys, "test").unwrap();
        write_index(dir.path(), &index, &load_signing_key(&secret).unwrap()).unwrap();
        let trusted = vec![parse_public_key(&fs::read_to_string(&public).unwrap()).unwrap()];
        let repo = Repository::open(dir.path().to_str().unwrap(), &trusted, true).unwrap();
        let path = repo.fetch(&repo.index.packages[0], &dir.path().join("cache")).unwrap();
        assert!(path.exists());

        // Tampering with the index breaks the signature.
        let tampered = fs::read_to_string(dir.path().join(INDEX_FILE)).unwrap().replace("friendly", "evil");
        fs::write(dir.path().join(INDEX_FILE), tampered).unwrap();
        assert!(Repository::open(dir.path().to_str().unwrap(), &trusted, true).is_err());
        // An unknown key is refused.
        let (_, other) = generate_key(&keys, "other").unwrap();
        let wrong = vec![parse_public_key(&fs::read_to_string(other).unwrap()).unwrap()];
        write_index(dir.path(), &index, &load_signing_key(&secret).unwrap()).unwrap();
        assert!(Repository::open(dir.path().to_str().unwrap(), &wrong, true).is_err());
    }

    #[test]
    fn search_and_closure() {
        let dir = tempfile::tempdir().unwrap();
        repo_with(dir.path(), &[("hello", "1.0")]);
        let mut index = Index::scan(dir.path()).unwrap();
        assert_eq!(index.search("greeter").len(), 1);
        assert_eq!(index.search("hello greeting").len(), 1);
        assert!(index.search("browser").is_empty());
        assert!(index.closure_problems().is_empty());
        index.packages[0].depends.libraries.push("libmissing.so.1".into());
        index.packages[0].depends.packages.push("nothing".into());
        assert_eq!(index.closure_problems().len(), 2);
    }

    #[test]
    fn tampered_package_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        repo_with(dir.path(), &[("hello", "1.0")]);
        let index = Index::scan(dir.path()).unwrap();
        let (secret, public) = generate_key(&dir.path().join("k"), "t").unwrap();
        write_index(dir.path(), &index, &load_signing_key(&secret).unwrap()).unwrap();
        let trusted = vec![parse_public_key(&fs::read_to_string(public).unwrap()).unwrap()];
        let repo = Repository::open(dir.path().to_str().unwrap(), &trusted, true).unwrap();
        let file = dir.path().join(&repo.index.packages[0].file);
        let mut bytes = fs::read(&file).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        fs::write(&file, bytes).unwrap();
        assert!(repo.fetch(&repo.index.packages[0], &dir.path().join("cache")).unwrap_err().contains("checksum"));
    }
}
