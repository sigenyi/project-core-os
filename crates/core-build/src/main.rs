use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use clap::{Parser, Subcommand};
use core_build::builder::{Builder, Outcome};
use core_build::recipe::{Recipe, load_ordered};
use core_build::source;
use core_pkg::repo::{Index, generate_key, load_signing_key, write_index};

#[derive(Parser)]
#[command(name = "core-build", about = "Build C.O.R.E. OS from source")]
struct Cli {
    /// Work directory (build root, repository, logs, stamps).
    #[arg(long, default_value = "/var/tmp/core-build")]
    work: PathBuf,
    /// Source tarball cache.
    #[arg(long, default_value = "/var/cache/core-build/sources")]
    cache: PathBuf,
    /// Directory holding bootstrap/ and recipes/.
    #[arg(long, default_value = "os")]
    recipes: PathBuf,
    /// Parallel make jobs.
    #[arg(long, short)]
    jobs: Option<usize>,
    /// Rebuild recipes even when they are up to date.
    #[arg(long)]
    force: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Download and verify every source.
    Fetch,
    /// Build the cross toolchain and the temporary tools (os/bootstrap).
    Bootstrap,
    /// Build every package of the base system (os/recipes).
    World {
        /// Start at this recipe (earlier ones must already be built).
        #[arg(long)]
        from: Option<String>,
    },
    /// Build the named recipes.
    Build { names: Vec<String> },
    /// Write the signed repository index (creates the key on first use).
    Index {
        /// Secret key; generated next to <name>.pub if missing.
        #[arg(long)]
        key: PathBuf,
    },
    /// Interactive shell inside the build root.
    Shell,
    /// Show which recipes are built.
    Status,
}

fn all_recipes(dir: &Path) -> Result<(Vec<Recipe>, Vec<Recipe>), String> {
    Ok((load_ordered(&dir.join("bootstrap"))?, load_ordered(&dir.join("recipes"))?))
}

fn run_list(b: &mut Builder, list: &[Recipe]) -> Result<(), String> {
    let total = list.len();
    for (i, r) in list.iter().enumerate() {
        let start = Instant::now();
        eprint!("[{}/{total}] {} ", i + 1, r.id());
        match b.build(r) {
            Ok(Outcome::UpToDate) => eprintln!("up to date"),
            Ok(Outcome::Built) => eprintln!("built in {:.0?}", start.elapsed()),
            Err(e) => {
                eprintln!("FAILED");
                return Err(format!("{}: {e}", r.id()));
            }
        }
    }
    Ok(())
}

fn run(cli: Cli) -> Result<(), String> {
    let jobs = cli.jobs.unwrap_or_else(|| std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1));
    let mut b = Builder::new(cli.work.clone(), cli.cache.clone(), jobs, cli.force);
    match cli.cmd {
        Cmd::Fetch => {
            let (boot, world) = all_recipes(&cli.recipes)?;
            let mut failed = 0;
            for r in boot.iter().chain(&world) {
                match source::fetch(r, &cli.cache) {
                    Ok(_) => println!("ok      {}", r.id()),
                    Err(e) => {
                        failed += 1;
                        println!("FAILED  {}: {e}", r.id());
                    }
                }
            }
            if failed > 0 {
                return Err(format!("{failed} recipes have missing sources"));
            }
        }
        Cmd::Bootstrap => run_list(&mut b, &load_ordered(&cli.recipes.join("bootstrap"))?)?,
        Cmd::World { from } => {
            let mut list = load_ordered(&cli.recipes.join("recipes"))?;
            if let Some(from) = from {
                let pos = list.iter().position(|r| r.package.name == from).ok_or(format!("no recipe {from}"))?;
                list.drain(..pos);
            }
            run_list(&mut b, &list)?
        }
        Cmd::Build { names } => {
            let (boot, world) = all_recipes(&cli.recipes)?;
            let mut list = Vec::new();
            for n in &names {
                let r = boot.iter().chain(&world).find(|r| &r.package.name == n).ok_or(format!("no recipe {n}"))?;
                list.push(r.clone());
            }
            run_list(&mut b, &list)?
        }
        Cmd::Index { key } => {
            if !key.exists() {
                let dir = key.parent().unwrap_or(Path::new("."));
                let name = key.file_stem().ok_or("bad key path")?.to_string_lossy().into_owned();
                let (secret, public) = generate_key(dir, &name)?;
                eprintln!("generated {} (public key {})", secret.display(), public.display());
            }
            let sk = load_signing_key(&key)?;
            let index = Index::scan(&b.repo())?;
            for p in index.closure_problems() {
                eprintln!("warning: {p}");
            }
            write_index(&b.repo(), &index, &sk)?;
            println!("indexed {} packages in {}", index.packages.len(), b.repo().display());
        }
        Cmd::Shell => b.shell()?,
        Cmd::Status => {
            let (boot, world) = all_recipes(&cli.recipes)?;
            for r in boot.iter().chain(&world) {
                let stamp = cli.work.join("state").join(&r.package.name);
                println!("{:8} {}", if stamp.exists() { "built" } else { "-" }, r.id());
            }
        }
    }
    b.unmount();
    Ok(())
}

fn main() -> ExitCode {
    env_logger_init();
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("core-build: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Minimal logger: `CORE_BUILD_LOG=debug` shows debug messages on stderr.
fn env_logger_init() {
    struct L(log::LevelFilter);
    impl log::Log for L {
        fn enabled(&self, m: &log::Metadata) -> bool {
            m.level() <= self.0
        }
        fn log(&self, r: &log::Record) {
            if self.enabled(r.metadata()) {
                eprintln!("{}: {}", r.level(), r.args());
            }
        }
        fn flush(&self) {}
    }
    let level = match std::env::var("CORE_BUILD_LOG").as_deref() {
        Ok("debug") => log::LevelFilter::Debug,
        Ok("info") => log::LevelFilter::Info,
        _ => log::LevelFilter::Warn,
    };
    let _ = log::set_boxed_logger(Box::new(L(level)));
    log::set_max_level(level);
}
