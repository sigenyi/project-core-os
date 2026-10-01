//! Fetching, verifying and unpacking pinned sources.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use core_pkg::archive::sha256_file;

use crate::recipe::{Recipe, Source};

/// Make sure every source of `recipe` is in the cache and matches its checksum.
pub fn fetch(recipe: &Recipe, cache: &Path) -> Result<Vec<PathBuf>, String> {
    fs::create_dir_all(cache).map_err(|e| format!("{}: {e}", cache.display()))?;
    recipe.sources.iter().map(|s| fetch_one(s, cache)).collect()
}

fn fetch_one(src: &Source, cache: &Path) -> Result<PathBuf, String> {
    let dest = cache.join(src.cache_name());
    if dest.exists() && sha256_file(&dest).ok().as_deref() == Some(src.sha256.as_str()) {
        return Ok(dest);
    }
    // Also accept the file under any URL's own name (e.g. a mirror's naming).
    for url in &src.urls {
        let alt = cache.join(url.rsplit('/').next().unwrap_or(""));
        if alt.is_file() && sha256_file(&alt).ok().as_deref() == Some(src.sha256.as_str()) {
            if alt != dest {
                fs::copy(&alt, &dest).map_err(|e| e.to_string())?;
            }
            return Ok(dest);
        }
    }
    let part = dest.with_extension("part");
    let mut errors = Vec::new();
    for url in &src.urls {
        let status = Command::new("curl")
            .args(["--fail", "--location", "--silent", "--show-error", "--retry", "3", "--output"])
            .arg(&part)
            .arg(url)
            .status()
            .map_err(|e| format!("cannot run curl: {e}"))?;
        if !status.success() {
            errors.push(format!("{url}: download failed"));
            continue;
        }
        let actual = sha256_file(&part).map_err(|e| e.to_string())?;
        if actual != src.sha256 {
            errors.push(format!("{url}: checksum {actual} does not match the recipe"));
            let _ = fs::remove_file(&part);
            continue;
        }
        fs::rename(&part, &dest).map_err(|e| e.to_string())?;
        return Ok(dest);
    }
    Err(format!("could not fetch {}:\n  {}", src.cache_name(), errors.join("\n  ")))
}

fn tar_extract(archive: &Path, into: &Path, strip: usize) -> Result<(), String> {
    fs::create_dir_all(into).map_err(|e| format!("{}: {e}", into.display()))?;
    let out = Command::new("tar")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(into)
        .arg(format!("--strip-components={strip}"))
        .arg("--no-same-owner")
        .output()
        .map_err(|e| format!("cannot run tar: {e}"))?;
    if !out.status.success() {
        return Err(format!("extracting {}: {}", archive.display(), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(())
}

/// Unpack all sources of a recipe into `tree` (created fresh).
pub fn unpack(recipe: &Recipe, cache: &Path, tree: &Path) -> Result<(), String> {
    if tree.exists() {
        fs::remove_dir_all(tree).map_err(|e| format!("{}: {e}", tree.display()))?;
    }
    fs::create_dir_all(tree).map_err(|e| e.to_string())?;
    for src in &recipe.sources {
        let file = cache.join(src.cache_name());
        let target = if src.dest.is_empty() { tree.to_path_buf() } else { tree.join(&src.dest) };
        if src.copy {
            fs::create_dir_all(&target).map_err(|e| e.to_string())?;
            fs::copy(&file, target.join(src.cache_name())).map_err(|e| e.to_string())?;
            continue;
        }
        match &src.inner {
            None => tar_extract(&file, &target, src.strip)?,
            Some(inner) => {
                let outer = tree.with_extension("outer");
                let _ = fs::remove_dir_all(&outer);
                tar_extract(&file, &outer, 1)?;
                let inner_path = outer.join(inner);
                if !inner_path.exists() {
                    return Err(format!("{} does not contain {inner}", file.display()));
                }
                tar_extract(&inner_path, &target, src.strip)?;
                fs::remove_dir_all(&outer).map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}
