//! cpkg: install, remove, upgrade and inspect C.O.R.E. OS packages.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use core_pkg::archive::{read_metadata, sha256_file};
use core_pkg::db::Db;
use core_pkg::manifest::FileKind;
use core_pkg::repo::{self, Index, Repository};
use core_pkg::resolve;
use core_pkg::transaction::{self, Options, Report};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Parser)]
#[command(name = "cpkg", version, about = "The C.O.R.E. OS package manager")]
struct Cli {
    /// Operate on another root directory (installation media, chroots, images).
    #[arg(long, global = true, default_value = "/")]
    root: PathBuf,
    /// Machine-readable output.
    #[arg(long, global = true)]
    json: bool,
    /// Use this repository (directory or URL) in addition to the configured ones.
    #[arg(long = "repo", global = true)]
    repos: Vec<String>,
    /// Trust repository indexes without checking their signature (development only).
    #[arg(long, global = true)]
    no_verify: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Install packages (names from repositories, or .cpk files) and their dependencies.
    Install {
        packages: Vec<String>,
        /// Replace files that exist but belong to no package (bootstrap only).
        #[arg(long)]
        overwrite_unowned: bool,
        #[arg(long)]
        reinstall: bool,
        /// Do not install dependencies.
        #[arg(long)]
        no_deps: bool,
        #[arg(long)]
        no_hooks: bool,
    },
    /// Remove installed packages.
    Remove {
        packages: Vec<String>,
        #[arg(long)]
        force: bool,
    },
    /// Upgrade installed packages (all, or the named ones).
    Upgrade { packages: Vec<String> },
    /// List installed packages.
    List,
    /// Show a package's details (installed, else from repositories).
    Info { package: String },
    /// List the files of an installed package.
    Files { package: String },
    /// Which installed package owns a file.
    Owns { path: String },
    /// Which package provides a program, library or name (installed or available).
    Provides { name: String },
    /// Search repositories by name, description, programs and keywords.
    Search { words: Vec<String> },
    /// Explain why a package is installed (what depends on it).
    Why { package: String },
    /// Check installed files against the database.
    Verify { packages: Vec<String> },
    /// Show the transaction history.
    History,
    /// Show the manifest of a .cpk file.
    Inspect { file: PathBuf },
    /// Create a .cpk from a manifest and a staged install tree.
    Pack {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        destdir: PathBuf,
        #[arg(long, default_value = ".")]
        out: PathBuf,
    },
    /// Write a signed index for a directory of packages.
    Index {
        dir: PathBuf,
        #[arg(long)]
        key: PathBuf,
        /// Fail if any dependency in the repository cannot be satisfied.
        #[arg(long)]
        check: bool,
    },
    /// Generate a repository signing key pair.
    Keygen {
        name: String,
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
}

#[derive(Deserialize, Default)]
struct RepoConfig {
    #[serde(default)]
    repo: Vec<RepoEntry>,
}

#[derive(Deserialize)]
struct RepoEntry {
    location: String,
}

struct Ctx {
    root: PathBuf,
    json: bool,
    repo_args: Vec<String>,
    verify: bool,
}

impl Ctx {
    fn repositories(&self) -> Result<Vec<Repository>, String> {
        let mut locations = self.repo_args.clone();
        let cfg = self.root.join("etc/cpkg/repos.toml");
        if let Ok(text) = std::fs::read_to_string(&cfg) {
            let parsed: RepoConfig = toml::from_str(&text).map_err(|e| format!("{}: {e}", cfg.display()))?;
            locations.extend(parsed.repo.into_iter().map(|r| r.location));
        }
        // Keys come from the target root, and from the host when installing into a
        // fresh root that has none yet.
        let mut trusted = repo::load_trusted_keys(&self.root.join("etc/cpkg/keys"));
        if self.root != Path::new("/") {
            trusted.extend(repo::load_trusted_keys(Path::new("/etc/cpkg/keys")));
        }
        locations.iter().map(|l| Repository::open(l, &trusted, self.verify)).collect()
    }

    fn cache(&self) -> PathBuf {
        self.root.join("var/cache/cpkg")
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    core_protocol::logging::init(0);
    let ctx = Ctx { root: cli.root.clone(), json: cli.json, repo_args: cli.repos.clone(), verify: !cli.no_verify };
    match run(&ctx, cli.cmd) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if ctx.json {
                println!("{}", json!({"error": e}));
            } else {
                eprintln!("cpkg: {e}");
            }
            ExitCode::FAILURE
        }
    }
}

fn print(ctx: &Ctx, value: Value, text: impl FnOnce() -> String) {
    if ctx.json {
        println!("{}", serde_json::to_string_pretty(&value).unwrap());
    } else {
        let t = text();
        if !t.is_empty() {
            println!("{t}");
        }
    }
}

fn print_report(ctx: &Ctx, report: &Report) {
    print(ctx, serde_json::to_value(report).unwrap(), || {
        let mut lines = Vec::new();
        for c in &report.installed {
            match &c.from {
                Some(from) => lines.push(format!("upgraded {from} -> {}", c.to.as_deref().unwrap_or("?"))),
                None => lines.push(format!("installed {}", c.to.as_deref().unwrap_or("?"))),
            }
        }
        for c in &report.removed {
            lines.push(format!("removed {}", c.from.as_deref().unwrap_or(&c.name)));
        }
        for n in &report.notes {
            lines.push(format!("note: {n}"));
        }
        lines.join("\n")
    });
}

fn run(ctx: &Ctx, cmd: Cmd) -> Result<(), String> {
    match cmd {
        Cmd::Install { packages, overwrite_unowned, reinstall, no_deps, no_hooks } => {
            let opts = Options { overwrite_unowned, force: false, run_hooks: !no_hooks };
            install(ctx, &packages, &opts, false, reinstall, no_deps)
        }
        Cmd::Upgrade { packages } => {
            install(ctx, &packages, &Options { run_hooks: true, ..Default::default() }, true, false, false)
        }
        Cmd::Remove { packages, force } => {
            let mut db = Db::open(&ctx.root)?;
            let _lock = db.lock()?;
            let opts = Options { force, run_hooks: true, ..Default::default() };
            let mut report = Report::default();
            let mut changed = Vec::new();
            for p in &packages {
                transaction::remove_package(&mut db, p, &packages, &opts, &mut report, &mut changed)?;
            }
            transaction::run_hooks(&db, &changed, &mut report);
            print_report(ctx, &report);
            Ok(())
        }
        Cmd::List => {
            let db = Db::open(&ctx.root)?;
            let list: Vec<Value> = db
                .packages
                .values()
                .map(|p| json!({"name": p.manifest.package.name, "version": p.manifest.version().to_string(), "summary": p.manifest.package.summary}))
                .collect();
            print(ctx, Value::Array(list), || {
                db.packages
                    .values()
                    .map(|p| {
                        format!(
                            "{:<24} {:<20} {}",
                            p.manifest.package.name,
                            p.manifest.version().to_string(),
                            p.manifest.package.summary
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            });
            Ok(())
        }
        Cmd::Info { package } => {
            let db = Db::open(&ctx.root)?;
            if let Some(p) = db.get(&package) {
                let m = &p.manifest;
                let dependents = db.dependents(&package);
                let mut v = serde_json::to_value(m).unwrap();
                v["installed"] = Value::Bool(true);
                v["required_by"] = json!(dependents);
                print(ctx, v, || {
                    format!(
                        "{} {}\n{}\ninstalled: yes ({} bytes)\nprograms: {}\nservices: {}\ndepends on: {}\nlibraries: {}\nrequired by: {}",
                        m.package.name,
                        m.version(),
                        m.package.summary,
                        m.package.installed_size,
                        m.provides.binaries.join(" "),
                        m.provides.services.join(" "),
                        m.depends.packages.join(" "),
                        m.depends.libraries.join(" "),
                        dependents.join(" ")
                    )
                });
                return Ok(());
            }
            let repos = ctx.repositories()?;
            let e = repos
                .iter()
                .find_map(|r| r.index.provider(&package))
                .ok_or_else(|| format!("no package named {package}"))?;
            let mut v = serde_json::to_value(e).unwrap();
            v["installed"] = Value::Bool(false);
            print(ctx, v, || {
                format!(
                    "{} {}\n{}\ninstalled: no\nprograms: {}",
                    e.name,
                    e.version(),
                    e.summary,
                    e.provides.binaries.join(" ")
                )
            });
            Ok(())
        }
        Cmd::Files { package } => {
            let db = Db::open(&ctx.root)?;
            let p = db.get(&package).ok_or_else(|| format!("{package} is not installed"))?;
            let paths: Vec<String> = p.files.iter().map(|f| format!("/{}", f.path)).collect();
            print(ctx, json!(paths), || paths.join("\n"));
            Ok(())
        }
        Cmd::Owns { path } => {
            let db = Db::open(&ctx.root)?;
            let rel = path.trim_start_matches('/').to_string();
            let owner = db
                .owners()
                .get(rel.as_str())
                .map(|s| s.to_string())
                .or_else(|| db.dir_users(&rel).first().map(|s| s.to_string()));
            match owner {
                Some(o) => {
                    print(ctx, json!({"path": path, "package": o}), || format!("{path} is owned by {o}"));
                    Ok(())
                }
                None => Err(format!("no installed package owns {path}")),
            }
        }
        Cmd::Provides { name } => {
            let db = Db::open(&ctx.root)?;
            let mut hits: Vec<Value> = db
                .packages
                .values()
                .filter(|p| {
                    let pr = &p.manifest.provides;
                    p.manifest.package.name == name
                        || pr.binaries.contains(&name)
                        || pr.libraries.contains(&name)
                        || pr.names.contains(&name)
                        || pr.services.contains(&name)
                })
                .map(|p| json!({"package": p.manifest.package.name, "installed": true}))
                .collect();
            if hits.is_empty() {
                for r in ctx.repositories().unwrap_or_default() {
                    for e in &r.index.packages {
                        let pr = &e.provides;
                        if e.name == name
                            || pr.binaries.contains(&name)
                            || pr.libraries.contains(&name)
                            || pr.names.contains(&name)
                            || pr.services.contains(&name)
                        {
                            hits.push(json!({"package": e.name, "installed": false, "summary": e.summary}));
                        }
                    }
                }
            }
            if hits.is_empty() {
                return Err(format!("nothing provides {name}"));
            }
            print(ctx, Value::Array(hits.clone()), || {
                hits.iter()
                    .map(|h| {
                        format!(
                            "{} ({})",
                            h["package"].as_str().unwrap_or(""),
                            if h["installed"] == true { "installed" } else { "available" }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            });
            Ok(())
        }
        Cmd::Search { words } => {
            let db = Db::open(&ctx.root)?;
            let repos = ctx.repositories()?;
            let query = words.join(" ");
            let mut results = Vec::new();
            for r in &repos {
                for e in r.index.search(&query) {
                    results.push(json!({"name": e.name, "version": e.version().to_string(), "summary": e.summary, "programs": e.provides.binaries, "installed": db.get(&e.name).is_some()}));
                }
            }
            print(ctx, Value::Array(results.clone()), || {
                results
                    .iter()
                    .map(|r| {
                        format!(
                            "{:<24} {}{}",
                            r["name"].as_str().unwrap_or(""),
                            r["summary"].as_str().unwrap_or(""),
                            if r["installed"] == true { " [installed]" } else { "" }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            });
            Ok(())
        }
        Cmd::Why { package } => {
            let db = Db::open(&ctx.root)?;
            db.get(&package).ok_or_else(|| format!("{package} is not installed"))?;
            // Breadth-first walk of reverse dependencies.
            let mut chains = Vec::new();
            let mut frontier = vec![vec![package.clone()]];
            let mut seen = BTreeSet::new();
            while let Some(chain) = frontier.pop() {
                let last = chain.last().unwrap().clone();
                let dependents = db.dependents(&last);
                if dependents.is_empty() && chain.len() > 1 {
                    chains.push(chain.clone());
                }
                for d in dependents {
                    if seen.insert(d.clone()) {
                        let mut next = chain.clone();
                        next.push(d);
                        frontier.push(next);
                    }
                }
            }
            print(ctx, json!({"package": package, "required_by_chains": chains}), || {
                if chains.is_empty() {
                    format!("nothing depends on {package}; it was installed explicitly or as part of the base system")
                } else {
                    chains.iter().map(|c| c.join(" <- ")).collect::<Vec<_>>().join("\n")
                }
            });
            Ok(())
        }
        Cmd::Verify { packages } => {
            let db = Db::open(&ctx.root)?;
            let mut problems = Vec::new();
            for (name, p) in &db.packages {
                if !packages.is_empty() && !packages.contains(name) {
                    continue;
                }
                for f in &p.files {
                    let path = ctx.root.join(&f.path);
                    match f.kind {
                        FileKind::File | FileKind::Hardlink => match sha256_file(&path) {
                            Err(_) => problems.push(json!({"package": name, "path": f.path, "problem": "missing"})),
                            Ok(h) if f.kind == FileKind::File && h != f.sha256 => {
                                let what = if f.is_config() { "modified configuration" } else { "modified" };
                                problems.push(json!({"package": name, "path": f.path, "problem": what}));
                            }
                            Ok(_) => {}
                        },
                        FileKind::Symlink | FileKind::Dir => {
                            if std::fs::symlink_metadata(&path).is_err() {
                                problems.push(json!({"package": name, "path": f.path, "problem": "missing"}));
                            }
                        }
                    }
                }
            }
            print(ctx, Value::Array(problems.clone()), || {
                if problems.is_empty() {
                    "all files intact".into()
                } else {
                    problems
                        .iter()
                        .map(|p| {
                            format!(
                                "{}: /{} {}",
                                p["package"].as_str().unwrap(),
                                p["path"].as_str().unwrap(),
                                p["problem"].as_str().unwrap()
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            });
            Ok(())
        }
        Cmd::History => {
            let db = Db::open(&ctx.root)?;
            let h = db.history();
            print(ctx, Value::Array(h.clone()), || {
                h.iter()
                    .map(|e| {
                        format!(
                            "{} {} {} {}",
                            e["ts"].as_str().unwrap_or(""),
                            e["op"].as_str().unwrap_or(""),
                            e["package"].as_str().unwrap_or(""),
                            e["to"].as_str().or(e["from"].as_str()).unwrap_or("")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            });
            Ok(())
        }
        Cmd::Inspect { file } => {
            let opened = read_metadata(&file)?;
            print(ctx, serde_json::to_value(&opened.manifest).unwrap(), || opened.manifest.to_toml());
            Ok(())
        }
        Cmd::Pack { manifest, destdir, out } => {
            let text = std::fs::read_to_string(&manifest).map_err(|e| format!("{}: {e}", manifest.display()))?;
            let m = core_pkg::Manifest::from_toml(&text)?;
            let path = core_pkg::create_package(&destdir, m, &out)?;
            print(ctx, json!({"package": path}), || format!("created {}", path.display()));
            Ok(())
        }
        Cmd::Index { dir, key, check } => {
            let index = Index::scan(&dir)?;
            let problems = index.closure_problems();
            if check && !problems.is_empty() {
                return Err(format!("repository is not self-contained:\n  {}", problems.join("\n  ")));
            }
            repo::write_index(&dir, &index, &repo::load_signing_key(&key)?)?;
            print(ctx, json!({"packages": index.packages.len(), "problems": problems}), || {
                let mut s = format!("indexed {} packages", index.packages.len());
                for p in &problems {
                    s.push_str(&format!("\nwarning: {p}"));
                }
                s
            });
            Ok(())
        }
        Cmd::Keygen { name, dir } => {
            let (secret, public) = repo::generate_key(&dir, &name)?;
            print(ctx, json!({"secret": secret, "public": public}), || {
                format!("secret key: {}\npublic key: {}", secret.display(), public.display())
            });
            Ok(())
        }
    }
}

fn install(
    ctx: &Ctx,
    packages: &[String],
    opts: &Options,
    upgrade: bool,
    reinstall: bool,
    no_deps: bool,
) -> Result<(), String> {
    let mut db = Db::open(&ctx.root)?;
    std::fs::create_dir_all(&ctx.root).map_err(|e| e.to_string())?;
    let _lock = db.lock()?;
    let (files, names): (Vec<&String>, Vec<&String>) = packages.iter().partition(|p| p.ends_with(".cpk"));
    let repos = if names.is_empty() && !upgrade && no_deps { Vec::new() } else { ctx.repositories()? };

    // Explicit .cpk files are installed as given; their dependencies come from repositories.
    let mut local: Vec<(String, PathBuf)> = Vec::new();
    let mut wanted: Vec<String> = names.iter().map(|s| s.to_string()).collect();
    for f in &files {
        let meta = read_metadata(Path::new(f))?;
        if !no_deps {
            wanted.extend(meta.manifest.depends.packages.iter().cloned());
            for lib in &meta.manifest.depends.libraries {
                if !meta.manifest.provides.libraries.contains(lib) && db.provides_library(lib).is_none() {
                    let provider = repos
                        .iter()
                        .find_map(|r| r.index.library_provider(lib))
                        .ok_or_else(|| format!("{f} needs library {lib}, which no repository provides"))?;
                    wanted.push(provider.name.clone());
                }
            }
        }
        local.push((meta.manifest.package.name.clone(), PathBuf::from(f)));
    }
    let local_names: Vec<&String> = local.iter().map(|(n, _)| n).collect();
    wanted.retain(|w| !local_names.contains(&w));

    let explicit: Vec<String> = names.iter().map(|s| s.to_string()).collect();
    let mut plan: Vec<resolve::Planned> = Vec::new();
    if no_deps {
        for w in &wanted {
            let p = repos
                .iter()
                .enumerate()
                .find_map(|(i, r)| r.index.provider(w).map(|e| resolve::Planned { repo: i, entry: e.clone() }))
                .ok_or_else(|| format!("no repository has {w}"))?;
            plan.push(p);
        }
    } else {
        if !explicit.is_empty() || upgrade {
            plan = resolve::plan(&repos, &db, &explicit, upgrade, reinstall)?;
        }
        // Dependencies of local .cpk files, unless already present.
        for dep in wanted.iter().filter(|w| !explicit.contains(w)) {
            if db.satisfies(dep) || plan.iter().any(|p| p.entry.satisfies(dep)) {
                continue;
            }
            for p in resolve::plan(&repos, &db, std::slice::from_ref(dep), false, false)? {
                if !plan.iter().any(|q| q.entry.name == p.entry.name) {
                    plan.push(p);
                }
            }
        }
    }

    let mut report = Report::default();
    let mut changed = Vec::new();
    for p in &plan {
        let path = repos[p.repo].fetch(&p.entry, &ctx.cache())?;
        transaction::install_package(&mut db, &path, opts, &mut report, &mut changed)?;
    }
    for (_, path) in &local {
        transaction::install_package(&mut db, path, opts, &mut report, &mut changed)?;
    }
    if opts.run_hooks {
        transaction::run_hooks(&db, &changed, &mut report);
    }
    if report.installed.is_empty() && report.notes.is_empty() {
        print(ctx, json!({"installed": []}), || "nothing to do".into());
    } else {
        print_report(ctx, &report);
    }
    Ok(())
}
