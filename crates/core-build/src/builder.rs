//! Building recipes in order: the cross stage on the host, the temporary tools and
//! the final packages inside the new root.
//!
//! Work directory layout:
//!
//! ```text
//! <work>/root/           the system being built (chroot for temp and final stages)
//! <work>/root/build/<n>/ per-recipe source tree (src/) and install tree (dest/)
//! <work>/repo/           finished .cpk packages
//! <work>/logs/<n>.log    build logs
//! <work>/state/<n>       stamp: hash of the recipe that was last built successfully
//! ```

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use core_pkg::archive::{create_package, sha256_bytes};
use core_pkg::db::Db;
use core_pkg::transaction::{self, Options, Report};

use crate::env::{Mounts, Place, command_in_root, run_script};
use crate::post;
use crate::recipe::{Recipe, Stage};
use crate::source;

/// The target triplet of the cross toolchain. The vendor field keeps it distinct
/// from the host's triplet, which is what forces a true cross build.
pub const TARGET: &str = "x86_64-core-linux-gnu";

pub struct Builder {
    pub work: PathBuf,
    pub cache: PathBuf,
    pub jobs: usize,
    /// Rebuild even if the stamp says the recipe is up to date.
    pub force: bool,
    /// Run recipes' test suites (`[build] check`) before packaging.
    pub check: bool,
    /// Timestamp recorded in packages and exported as SOURCE_DATE_EPOCH.
    pub epoch: u64,
    mounts: Option<Mounts>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Built,
    UpToDate,
}

fn io(path: &Path) -> impl Fn(std::io::Error) -> String + '_ {
    move |e| format!("{}: {e}", path.display())
}

impl Builder {
    pub fn new(work: PathBuf, cache: PathBuf, jobs: usize, force: bool, check: bool) -> Builder {
        let epoch = std::env::var("SOURCE_DATE_EPOCH")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0));
        Builder { work, cache, jobs, force, check, epoch, mounts: None }
    }

    pub fn root(&self) -> PathBuf {
        self.work.join("root")
    }

    pub fn repo(&self) -> PathBuf {
        self.work.join("repo")
    }

    fn stamp_path(&self, r: &Recipe) -> PathBuf {
        self.work.join("state").join(&r.package.name)
    }

    /// Records that the recipe's test suite passed for this recipe hash.
    fn checked_path(&self, r: &Recipe) -> PathBuf {
        self.work.join("state").join(format!("{}.checked", r.package.name))
    }

    fn wants_check(&self, r: &Recipe) -> bool {
        self.check && r.build.stage == Stage::Final && r.build.check.is_some()
    }

    fn recipe_hash(r: &Recipe) -> Result<String, String> {
        let mut text = fs::read(&r.path).map_err(io(&r.path))?;
        // Files brought in from the repository count as part of the recipe.
        for rel in &r.build.files {
            let out = Command::new("tar")
                .args(["--sort=name", "--mtime=@0", "--owner=0", "--group=0", "--numeric-owner", "-cf", "-", "-C"])
                .arg(r.local_file(rel).parent().unwrap())
                .arg(r.local_file(rel).file_name().unwrap())
                .output()
                .map_err(|e| e.to_string())?;
            text.extend_from_slice(&out.stdout);
        }
        Ok(sha256_bytes(&text))
    }

    fn up_to_date(&self, r: &Recipe) -> Result<bool, String> {
        if self.force {
            return Ok(false);
        }
        let want = Self::recipe_hash(r)?;
        let have = fs::read_to_string(self.stamp_path(r)).unwrap_or_default();
        if have.trim() != want {
            return Ok(false);
        }
        if self.wants_check(r) && fs::read_to_string(self.checked_path(r)).unwrap_or_default().trim() != want {
            return Ok(false);
        }
        if r.build.stage == Stage::Final {
            // The package must still be in the repository and installed at that
            // version.
            if !self.repo().join(r.manifest(0).file_name()).is_file() {
                return Ok(false);
            }
            let db = Db::open(&self.root())?;
            let id = format!("{}-{}", r.package.version, r.package.release);
            return Ok(db
                .get(&r.package.name)
                .is_some_and(|i| format!("{}-{}", i.manifest.package.version, i.manifest.package.release) == id));
        }
        Ok(true)
    }

    fn write_stamp(&self, r: &Recipe, hash: &str) -> Result<(), String> {
        let path = self.stamp_path(r);
        fs::create_dir_all(path.parent().unwrap()).map_err(io(&path))?;
        fs::write(&path, format!("{hash}\n")).map_err(io(&path))
    }

    fn base_env(&self) -> Vec<(String, String)> {
        vec![
            ("HOME".into(), "/root".into()),
            ("TERM".into(), "dumb".into()),
            ("MAKEFLAGS".into(), format!("-j{}", self.jobs)),
            ("JOBS".into(), self.jobs.to_string()),
            ("SOURCE_DATE_EPOCH".into(), self.epoch.to_string()),
            ("CORE_TGT".into(), TARGET.into()),
        ]
    }

    fn ensure_mounts(&mut self) -> Result<(), String> {
        if self.mounts.is_none() {
            let root = self.root();
            for needed in ["usr/bin/env", "usr/bin/bash"] {
                if !root.join(needed).exists() {
                    return Err(format!("the build root has no /{needed} yet; run the cross stage first"));
                }
            }
            self.mounts = Some(Mounts::setup(&root)?);
        }
        Ok(())
    }

    /// Release the virtual filesystems mounted in the build root.
    pub fn unmount(&mut self) {
        self.mounts = None;
    }

    pub fn build(&mut self, r: &Recipe) -> Result<Outcome, String> {
        if self.up_to_date(r)? {
            return Ok(Outcome::UpToDate);
        }
        // The stamp records the recipe as it was when the build started, even if
        // the file is edited while it builds.
        let hash = Self::recipe_hash(r)?;
        // Until this build succeeds, the recipe is not built (or tested).
        let _ = fs::remove_file(self.stamp_path(r));
        let _ = fs::remove_file(self.checked_path(r));
        source::fetch(r, &self.cache)?;
        let root = self.root();
        fs::create_dir_all(&root).map_err(io(&root))?;
        let name = &r.package.name;
        let build_rel = format!("build/{name}");
        let build_dir = root.join(&build_rel);
        let src = build_dir.join("src");
        let dest = build_dir.join("dest");
        if build_dir.exists() {
            fs::remove_dir_all(&build_dir).map_err(io(&build_dir))?;
        }
        source::unpack(r, &self.cache, &src)?;
        fs::create_dir_all(&dest).map_err(io(&dest))?;
        let log = self.work.join("logs").join(format!("{name}.log"));
        let mut env = self.base_env();

        match r.build.stage {
            Stage::Cross => {
                let root_s = root.to_string_lossy().into_owned();
                env.extend([
                    ("LFS".into(), root_s.clone()),
                    ("LFS_TGT".into(), TARGET.into()),
                    ("PATH".into(), format!("{root_s}/tools/bin:/usr/bin:/bin")),
                    ("CONFIG_SITE".into(), format!("{root_s}/usr/share/config.site")),
                    ("LC_ALL".into(), "POSIX".into()),
                    ("SRC".into(), src.to_string_lossy().into_owned()),
                    ("HOME".into(), self.work.join("home").to_string_lossy().into_owned()),
                ]);
                fs::create_dir_all(self.work.join("home")).map_err(|e| e.to_string())?;
                run_script(Place::Host { cwd: &src }, &r.build.script, &env, &log)?;
            }
            Stage::Temp => {
                self.ensure_mounts()?;
                env.extend([
                    ("PATH".into(), "/usr/bin:/usr/sbin".into()),
                    ("LC_ALL".into(), "POSIX".into()),
                    ("SRC".into(), format!("/{build_rel}/src")),
                ]);
                let cwd = format!("/{build_rel}/src");
                run_script(Place::Chroot { root: &root, cwd: &cwd }, &r.build.script, &env, &log)?;
            }
            Stage::Final => {
                self.ensure_mounts()?;
                env.extend([
                    ("PATH".into(), "/usr/bin:/usr/sbin".into()),
                    ("SRC".into(), format!("/{build_rel}/src")),
                    ("DESTDIR".into(), format!("/{build_rel}/dest")),
                ]);
                let cwd = format!("/{build_rel}/src");
                run_script(Place::Chroot { root: &root, cwd: &cwd }, &r.build.script, &env, &log)?;
                if self.wants_check(r) {
                    self.run_check(r, &build_rel, &env)?;
                }
                self.package_and_install(r, &dest, &log)?;
            }
        }
        fs::remove_dir_all(&build_dir).map_err(io(&build_dir))?;
        self.write_stamp(r, &hash)?;
        if self.wants_check(r) {
            fs::write(self.checked_path(r), format!("{hash}\n")).map_err(io(&self.checked_path(r)))?;
        }
        Ok(Outcome::Built)
    }

    /// Run the recipe's test suite in its build tree. Whatever it leaves in
    /// `$RESULTS` is copied to `<work>/logs/<name>-check/`, pass or fail.
    fn run_check(&self, r: &Recipe, build_rel: &str, env: &[(String, String)]) -> Result<(), String> {
        let root = self.root();
        let name = &r.package.name;
        let results_rel = format!("{build_rel}/results");
        let results = root.join(&results_rel);
        fs::create_dir_all(&results).map_err(io(&results))?;
        // SOURCE_DATE_EPOCH makes tools stamp fixed times (binutils' ar then
        // cannot tell older members from newer ones); tests see a normal system.
        let mut env: Vec<(String, String)> = env.iter().filter(|(k, _)| k != "SOURCE_DATE_EPOCH").cloned().collect();
        env.push(("RESULTS".into(), format!("/{results_rel}")));
        let log = self.work.join("logs").join(format!("{name}.check.log"));
        let cwd = format!("/{build_rel}/src");
        let script = r.build.check.as_deref().unwrap_or_default();
        let outcome = run_script(Place::Chroot { root: &root, cwd: &cwd }, script, &env, &log);
        let saved = self.work.join("logs").join(format!("{name}-check"));
        let _ = fs::remove_dir_all(&saved);
        let copied = Command::new("cp").arg("-a").arg(&results).arg(&saved).status();
        if !copied.is_ok_and(|s| s.success()) {
            log::warn!("{name}: could not save test results to {}", saved.display());
        }
        outcome.map_err(|e| format!("test suite failed (results in {}): {e}", saved.display()))
    }

    fn package_and_install(&self, r: &Recipe, dest: &Path, log: &Path) -> Result<(), String> {
        let root = self.root();
        if r.build.merge_usr {
            post::normalize_merged_usr(dest).map_err(|e| format!("{}: {e}", r.package.name))?;
        }
        post::remove_clutter(dest)?;
        if r.build.strip {
            let n = post::strip(dest, &root)?;
            log::info!("{}: stripped {n} files", r.package.name);
        }
        let repo = self.repo();
        let pkg = create_package(dest, r.manifest(self.epoch), &repo)?;
        // Then drop older builds of this package (other versions) from the repository.
        if let Ok(rd) = fs::read_dir(&repo) {
            let prefix = format!("{}-", r.package.name);
            for e in rd.filter_map(|e| e.ok()) {
                let file = e.file_name().to_string_lossy().into_owned();
                if e.path() != pkg
                    && file.starts_with(&prefix)
                    && file.ends_with(".cpk")
                    && core_pkg::archive::read_metadata(&e.path())
                        .is_ok_and(|m| m.manifest.package.name == r.package.name)
                {
                    fs::remove_file(e.path()).map_err(|e| e.to_string())?;
                }
            }
        }
        check_ai_metadata(&pkg)?;
        let mut db = Db::open(&root)?;
        let _lock = db.lock()?;
        let opts = Options { overwrite_unowned: true, force: false, run_hooks: true };
        let mut report = Report::default();
        let mut changed = Vec::new();
        transaction::install_package(&mut db, &pkg, &opts, &mut report, &mut changed)?;
        transaction::run_hooks(&db, &changed, &mut report);
        if let Ok(mut f) = fs::OpenOptions::new().append(true).open(log) {
            let _ = writeln!(f, "\n== packaged {} and installed it into the build root", pkg.display());
            for n in &report.notes {
                let _ = writeln!(f, "note: {n}");
            }
        }
        Ok(())
    }

    /// Run an interactive shell inside the build root.
    pub fn shell(&mut self) -> Result<(), String> {
        self.ensure_mounts()?;
        let mut cmd = command_in_root(&self.root(), "/usr/bin/bash")?;
        cmd.arg("--login").env_clear().envs(self.base_env());
        cmd.env("PATH", "/usr/bin:/usr/sbin").env("PS1", "(core-build) \\w \\$ ");
        cmd.status().map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// The AI launches programs by the name a package declares, so that name must be a
/// program the package really installs.
fn check_ai_metadata(pkg: &Path) -> Result<(), String> {
    let m = core_pkg::archive::read_metadata(pkg)?.manifest;
    let launch = &m.ai.launch;
    if !launch.is_empty() && !m.provides.binaries.iter().any(|b| b == launch) {
        let _ = fs::remove_file(pkg);
        return Err(format!(
            "[ai] launch = {launch:?}, but {} installs no such program (it provides: {})",
            m.package.name,
            m.provides.binaries.join(" ")
        ));
    }
    Ok(())
}
