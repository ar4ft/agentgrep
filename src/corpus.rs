use anyhow::{Context, Result};
use globset::{GlobBuilder, GlobSetBuilder};
use ignore::WalkBuilder;
use rayon::prelude::*;
use std::io::Read;
use std::path::{Path, PathBuf};

pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

pub fn is_binary_format(path: &Path) -> bool {
    let extension = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        extension.as_str(),
        "pdf"
            | "doc"
            | "docx"
            | "xls"
            | "xlsx"
            | "ppt"
            | "pptx"
            | "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "heic"
            | "ico"
            | "zip"
            | "gz"
            | "tar"
            | "7z"
            | "woff"
            | "woff2"
            | "ttf"
            | "mp3"
            | "mp4"
            | "mov"
            | "sqlite"
            | "db"
            | "dylib"
            | "so"
            | "a"
            | "o"
            | "wasm"
    )
}

#[derive(Debug)]
pub struct Source {
    pub path: String,
    pub content: String,
}

pub fn canonical_root(path: &Path) -> Result<PathBuf> {
    let root = path
        .canonicalize()
        .with_context(|| format!("Cannot open root {}", path.display()))?;
    anyhow::ensure!(root.is_dir(), "Search root must be a directory");
    Ok(root)
}

pub fn files(root: &Path, globs: &[String], hidden: bool) -> Result<(Vec<PathBuf>, Vec<String>)> {
    let mut includes = GlobSetBuilder::new();
    let mut excludes = GlobSetBuilder::new();
    let mut has_includes = false;
    for glob in globs {
        let (exclude, pattern) = glob
            .strip_prefix('!')
            .map_or((false, glob.as_str()), |s| (true, s));
        let pattern = if !pattern.contains('/') {
            format!("**/{pattern}")
        } else if pattern.ends_with('/') {
            format!("{pattern}**")
        } else {
            pattern.to_owned()
        };
        let matcher = GlobBuilder::new(&pattern).literal_separator(true).build()?;
        if exclude {
            excludes.add(matcher);
        } else {
            includes.add(matcher);
            has_includes = true;
        }
    }
    let includes = includes.build()?;
    let excludes = excludes.build()?;
    let mut walker = WalkBuilder::new(root);
    walker
        .hidden(!hidden)
        .follow_links(false)
        .require_git(false);
    // These are always excluded, even when ignored files/hidden files are requested.
    walker.filter_entry(|entry| {
        let n = entry.file_name().to_string_lossy();
        !matches!(
            n.as_ref(),
            ".git"
                | "node_modules"
                | "target"
                | ".venv"
                | "vendor"
                | "dist"
                | "build"
                | ".agentgrep"
        )
    });
    let mut paths = Vec::new();
    let mut warnings = Vec::new();
    for entry in walker.build() {
        match entry {
            Ok(entry) if entry.file_type().is_some_and(|t| t.is_file()) => {
                if is_binary_format(entry.path()) {
                    continue;
                }
                let relative = entry.path().strip_prefix(root)?;
                if (!has_includes || includes.is_match(relative)) && !excludes.is_match(relative) {
                    if entry.metadata().is_ok_and(|m| m.len() > MAX_FILE_BYTES) {
                        warnings.push(format!(
                            "Skipped {}: file exceeds 2 MiB",
                            relative.display()
                        ));
                        continue;
                    }
                    paths.push(entry.into_path());
                }
            }
            Ok(_) => {}
            Err(e) => warnings.push(e.to_string()),
        }
    }
    paths.sort();
    Ok((paths, warnings))
}

pub fn load(root: &Path, globs: &[String], hidden: bool) -> Result<(Vec<Source>, Vec<String>)> {
    let (paths, mut warnings) = files(root, globs, hidden)?;
    let loaded: Vec<_> = paths
        .par_iter()
        .map(|path| {
            let canonical = path.canonicalize()?;
            anyhow::ensure!(
                canonical.starts_with(root),
                "Source path escaped root: {}",
                path.display()
            );
            let mut bytes = Vec::new();
            std::fs::File::open(path)
                .with_context(|| format!("Cannot read {}", path.display()))?
                .take(MAX_FILE_BYTES + 1)
                .read_to_end(&mut bytes)?;
            // Files may grow between traversal and read. Reject non-UTF8 and binaries.
            if bytes.len() as u64 > MAX_FILE_BYTES || bytes.contains(&0) {
                anyhow::bail!(
                    "Skipped {}: oversized or binary source",
                    path.strip_prefix(root)?.display()
                );
            }
            match String::from_utf8(bytes) {
                Ok(content) => Ok(Some(Source {
                    path: path
                        .strip_prefix(root)?
                        .to_str()
                        .context("Source path is not UTF-8")?
                        .to_owned(),
                    content,
                })),
                Err(_) => anyhow::bail!(
                    "Skipped {}: source is not UTF-8",
                    path.strip_prefix(root)?.display()
                ),
            }
        })
        .collect::<Vec<Result<Option<Source>>>>();
    let mut sources = Vec::new();
    for result in loaded {
        match result {
            Ok(Some(s)) => sources.push(s),
            Ok(None) => {}
            Err(e) => warnings.push(e.to_string()),
        }
    }
    Ok((sources, warnings))
}
