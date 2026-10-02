//! Recipes: how to build one piece of the OS from pinned sources.
//!
//! ```toml
//! [package]
//! name = "sed"
//! version = "4.9"
//! release = 1
//! summary = "GNU stream editor"
//! license = "GPL-3.0-or-later"
//! homepage = "https://www.gnu.org/software/sed/"
//!
//! [[source]]
//! sha256 = "6e226b73…"
//! urls = ["https://ftp.gnu.org/gnu/sed/sed-4.9.tar.xz",
//!         "http://archive.ubuntu.com/ubuntu/pool/main/s/sed/sed_4.9.orig.tar.xz"]
//!
//! [build]
//! stage = "final"        # cross | temp | final
//! script = '''   # literal string: no escape processing
//! ./configure --prefix=/usr
//! make
//! make DESTDIR=$DESTDIR install
//! '''
//!
//! [runtime]
//! depends = ["glibc"]
//!
//! [ai]
//! kind = "cli"
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use core_pkg::manifest::{AiInfo, Depends, Manifest, PackageInfo, Provides, validate_name};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    /// Cross toolchain and cross-compiled temporary tools, built on the host into the
    /// new root. Not packaged.
    Cross,
    /// Temporary tools built inside the chroot. Not packaged.
    Temp,
    /// Real system packages: built in the chroot, packaged, installed with cpkg.
    #[default]
    Final,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub package: RecipePackage,
    #[serde(default, rename = "source")]
    pub sources: Vec<Source>,
    pub build: Build,
    #[serde(default)]
    pub runtime: Runtime,
    #[serde(default)]
    pub ai: AiInfo,
    #[serde(skip)]
    pub path: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipePackage {
    pub name: String,
    pub version: String,
    #[serde(default = "one")]
    pub release: u32,
    pub summary: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub homepage: String,
}

fn one() -> u32 {
    1
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub sha256: String,
    /// Tried in order; the first is the upstream location.
    pub urls: Vec<String>,
    /// Name in the source cache (default: last URL component of the first URL).
    #[serde(default)]
    pub file: Option<String>,
    /// A tarball inside this one that holds the real source (some mirrors wrap the
    /// upstream release).
    #[serde(default)]
    pub inner: Option<String>,
    /// Subdirectory of the source tree to extract into ("" = the tree itself).
    #[serde(default)]
    pub dest: String,
    /// Leading path components to strip (default 1).
    #[serde(default = "one_usize")]
    pub strip: usize,
    /// Copy the file instead of extracting it (patches, single files).
    #[serde(default)]
    pub copy: bool,
}

fn one_usize() -> usize {
    1
}

impl Source {
    pub fn cache_name(&self) -> String {
        self.file
            .clone()
            .unwrap_or_else(|| self.urls.first().and_then(|u| u.rsplit('/').next()).unwrap_or("source").to_string())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Build {
    #[serde(default)]
    pub stage: Stage,
    pub script: String,
    /// Test suite, run with `core-build --check` in the build tree after `script`
    /// and before packaging, so the packaged binaries are the tested ones. It must
    /// fail on any unexpected test result; results go in `$RESULTS`.
    #[serde(default)]
    pub check: Option<String>,
    /// Strip binaries and libraries in the staged tree (final stage).
    #[serde(default = "yes")]
    pub strip: bool,
    /// Move /bin, /sbin, /lib… into /usr (off only for the package that owns the
    /// compatibility symlinks themselves).
    #[serde(default = "yes")]
    pub merge_usr: bool,
    /// Recipes that must be built first (ordering only; the build root accumulates
    /// everything built before).
    #[serde(default)]
    pub after: Vec<String>,
    /// Files or directories from this repository (relative to the recipe) copied
    /// into the source tree: configuration, our own programs.
    #[serde(default)]
    pub files: Vec<String>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Runtime {
    #[serde(default)]
    pub depends: Vec<String>,
    /// Extra names the package answers to.
    #[serde(default)]
    pub provides: Vec<String>,
}

impl Recipe {
    pub fn load(path: &Path) -> Result<Recipe, String> {
        let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut r: Recipe = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        r.path = path.to_path_buf();
        r.validate().map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(r)
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_name(&self.package.name)?;
        for s in &self.sources {
            if s.sha256.len() != 64 || !s.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(format!("source {} has no valid sha256", s.cache_name()));
            }
            if s.urls.is_empty() {
                return Err("a source needs at least one URL".into());
            }
            if s.dest.split('/').any(|c| c == "..") || s.dest.starts_with('/') {
                return Err(format!("bad source dest {:?}", s.dest));
            }
        }
        for f in &self.build.files {
            if !self.local_file(f).exists() {
                return Err(format!("file {f} does not exist"));
            }
        }
        if self.build.stage == Stage::Final && self.package.summary.trim().is_empty() {
            return Err("final packages need a summary".into());
        }
        Ok(())
    }

    pub fn local_file(&self, rel: &str) -> PathBuf {
        self.path.parent().unwrap_or(Path::new(".")).join(rel)
    }

    pub fn id(&self) -> String {
        format!("{}-{}-{}", self.package.name, self.package.version, self.package.release)
    }

    /// The package manifest (provides and library dependencies are filled in later
    /// from the payload).
    pub fn manifest(&self, build_date: u64) -> Manifest {
        Manifest {
            package: PackageInfo {
                name: self.package.name.clone(),
                version: self.package.version.clone(),
                release: self.package.release,
                arch: "x86_64".into(),
                summary: self.package.summary.clone(),
                description: self.package.description.clone(),
                license: self.package.license.clone(),
                homepage: self.package.homepage.clone(),
                installed_size: 0,
                build_date,
            },
            depends: Depends { packages: self.runtime.depends.clone(), libraries: Vec::new() },
            provides: Provides { names: self.runtime.provides.clone(), ..Default::default() },
            ai: self.ai.clone(),
        }
    }
}

/// Load the recipes listed (one name per line, `#` comments) in `dir/ORDER`.
pub fn load_ordered(dir: &Path) -> Result<Vec<Recipe>, String> {
    let order = dir.join("ORDER");
    let text = fs::read_to_string(&order).map_err(|e| format!("{}: {e}", order.display()))?;
    let mut recipes = Vec::new();
    for line in text.lines() {
        let name = line.split('#').next().unwrap_or("").trim();
        if name.is_empty() {
            continue;
        }
        let r = Recipe::load(&dir.join(format!("{name}.toml")))?;
        recipes.push(r);
    }
    // Every recipe file must be listed exactly once, so nothing is silently skipped.
    let mut listed: Vec<String> =
        recipes.iter().map(|r| r.path.file_stem().unwrap().to_string_lossy().into_owned()).collect();
    listed.sort();
    for w in listed.windows(2) {
        if w[0] == w[1] {
            return Err(format!("{} appears twice in {}", w[0], order.display()));
        }
    }
    for e in fs::read_dir(dir).map_err(|e| e.to_string())?.filter_map(|e| e.ok()) {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "toml") {
            let stem = p.file_stem().unwrap().to_string_lossy().into_owned();
            if listed.binary_search(&stem).is_err() {
                return Err(format!("{} is not listed in {}", p.display(), order.display()));
            }
        }
    }
    Ok(recipes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SED: &str = r#"
[package]
name = "sed"
version = "4.9"
summary = "GNU stream editor"

[[source]]
sha256 = "6e226b732e1cd739464ad6862bd1a1aba42d7982922da7a53519631d24975181"
urls = ["https://ftp.gnu.org/gnu/sed/sed-4.9.tar.xz"]

[build]
script = "make"

[runtime]
depends = ["glibc"]
"#;

    #[test]
    fn parses_and_builds_manifest() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("sed.toml"), SED).unwrap();
        let r = Recipe::load(&dir.path().join("sed.toml")).unwrap();
        assert_eq!(r.id(), "sed-4.9-1");
        assert_eq!(r.build.stage, Stage::Final);
        assert_eq!(r.sources[0].cache_name(), "sed-4.9.tar.xz");
        let m = r.manifest(0);
        assert_eq!(m.depends.packages, ["glibc"]);
        m.validate().unwrap();
    }

    #[test]
    fn check_script_is_optional() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("sed.toml"), SED).unwrap();
        assert!(Recipe::load(&dir.path().join("sed.toml")).unwrap().build.check.is_none());
        let with_check = SED.replace("script = \"make\"", "script = \"make\"\ncheck = \"make check\"");
        fs::write(dir.path().join("sed.toml"), with_check).unwrap();
        let r = Recipe::load(&dir.path().join("sed.toml")).unwrap();
        assert_eq!(r.build.check.as_deref(), Some("make check"));
    }

    #[test]
    fn order_must_cover_every_recipe() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("sed.toml"), SED).unwrap();
        fs::write(dir.path().join("grep.toml"), SED.replace("\"sed\"", "\"grep\"")).unwrap();
        fs::write(dir.path().join("ORDER"), "sed # first\n").unwrap();
        assert!(load_ordered(dir.path()).unwrap_err().contains("grep.toml is not listed"));
        fs::write(dir.path().join("ORDER"), "sed\ngrep\n").unwrap();
        assert_eq!(load_ordered(dir.path()).unwrap().len(), 2);
    }

    #[test]
    fn rejects_bad_sources() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("x.toml"), SED.replace("6e226b73", "zz")).unwrap();
        assert!(Recipe::load(&dir.path().join("x.toml")).is_err());
    }
}
