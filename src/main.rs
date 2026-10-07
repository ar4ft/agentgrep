mod corpus;
mod index;
mod mcp;
mod model;
mod search;
mod structure;
mod update;
mod worker;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use std::{io::Write, path::PathBuf, process::Command};

#[derive(Parser)]
#[command(
    name = "agx",
    version,
    about = "Local source evidence for agents. Exact, structural, ranked, and hybrid search."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Clone, ValueEnum)]
enum Mode {
    Text,
    Symbol,
    Ranked,
    Hybrid,
}
#[derive(Clone, ValueEnum)]
enum Harness {
    Codex,
    Claude,
    Both,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the local editor worker (text, symbol, lexical BM25 only).
    Serve {
        #[arg(long, required = true)]
        stdio: bool,
        /// Explicitly select the editor-only capability surface (also the default).
        #[arg(long)]
        restricted: bool,
    },
    /// Find source evidence. JSON is the default output.
    Search {
        query: String,
        #[arg(default_value = ".")]
        root: PathBuf,
        #[arg(long, value_enum, default_value = "text")]
        mode: Mode,
        #[arg(short = 'g', long)]
        glob: Vec<String>,
        #[arg(long)]
        hidden: bool,
        #[arg(short = 'F', long)]
        literal: bool,
        #[arg(long)]
        case_sensitive: bool,
        #[arg(short = 'C', long, default_value_t = 2)]
        context: usize,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long, default_value_t = 16000)]
        budget_bytes: usize,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        pretty: bool,
    },
    /// Refresh the local index; reads current files and reuses unchanged parses.
    Index {
        #[arg(default_value = ".")]
        root: PathBuf,
    },
    /// Return compact repository orientation as JSON.
    Map {
        #[arg(default_value = ".")]
        root: PathBuf,
    },
    /// Read a source range, constrained to root.
    Read {
        path: String,
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long, default_value_t = 1)]
        start: usize,
        #[arg(long, default_value_t = 100)]
        end: usize,
        #[arg(long, default_value_t = 16000)]
        budget_bytes: usize,
    },
    /// Start an MCP server over newline-delimited JSON-RPC on stdin/stdout.
    Mcp {
        #[arg(long, default_value = ".")]
        root: PathBuf,
    },
    /// Install the bundled skill without replacing harness instructions.
    Skill {
        #[arg(long, value_enum, default_value = "both")]
        harness: Harness,
        #[arg(long)]
        global: bool,
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long)]
        force: bool,
    },
    /// Convert a document to searchable Markdown with local LiteParse.
    Parse {
        input: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Show capability and optional dependency status.
    Doctor,
    /// Remove this root's local index and embedding cache.
    Clean {
        #[arg(default_value = ".")]
        root: PathBuf,
    },
    /// Check, install, or roll back verified releases; configure macOS automatic updates.
    Update {
        #[command(subcommand)]
        action: update::Action,
    },
    /// Explicitly update to the newest unsigned development prerelease (GitHub SHA-256 only).
    UpdatePre {
        /// Report the newest available development archive without installing it.
        #[arg(long)]
        check: bool,
    },
}

fn install_skill(harness: Harness, global: bool, root: PathBuf, force: bool) -> Result<()> {
    let base = if global {
        directories::BaseDirs::new()
            .context("Cannot locate home")?
            .home_dir()
            .to_path_buf()
    } else {
        corpus::canonical_root(&root)?
    };
    let folders: &[&str] = match harness {
        Harness::Codex => &[".agents"],
        Harness::Claude => &[".claude"],
        Harness::Both => &[".agents", ".claude"],
    };
    let content = include_str!("../skills/agentgrep/SKILL.md");
    let paths: Vec<_> = folders
        .iter()
        .map(|f| base.join(f).join("skills/agentgrep/SKILL.md"))
        .collect();
    // Preflight all destinations to avoid half-installing when one has custom content.
    for path in &paths {
        if path.exists() && std::fs::read_to_string(path)? != content && !force {
            anyhow::bail!(
                "{} already contains a different skill; use --force to replace",
                path.display()
            );
        }
    }
    for path in paths {
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, content)?;
        println!("Installed {}", path.display());
    }
    Ok(())
}

fn parse_document(input: PathBuf, out: PathBuf) -> Result<()> {
    let input = input.canonicalize().context("Cannot locate document")?;
    anyhow::ensure!(!out.exists(), "Output exists; choose a new output path");
    let parent = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    let temporary = tempfile::NamedTempFile::new_in(parent)?;
    // Child stdout goes to stderr: CLI stdout remains one JSON result.
    let status = Command::new("lit").arg("parse").arg(&input).args(["--format","markdown","--image-mode","off","-o"]).arg(temporary.path()).stdout(std::process::Stdio::from(std::io::stderr())).stderr(std::process::Stdio::inherit()).status().context("LiteParse is not installed. Install @llamaindex/liteparse with npm, or liteparse with pip")?;
    anyhow::ensure!(status.success(), "LiteParse failed ({status})");
    let text = std::fs::read_to_string(temporary.path())?;
    anyhow::ensure!(
        !text.trim().is_empty(),
        "LiteParse produced no searchable text"
    );
    temporary.persist_noclobber(&out).map_err(|e| e.error)?;
    println!(
        "{}",
        serde_json::json!({"source":input,"extracted":out,"format":"markdown","citation_scope":"Search line numbers refer to extracted Markdown, not original document pages"})
    );
    Ok(())
}

fn run() -> Result<()> {
    match Cli::parse().command {
        Commands::Serve { .. } => worker::serve()?,
        Commands::Search {
            query,
            root,
            mode,
            glob,
            hidden,
            literal,
            case_sensitive,
            context,
            limit,
            budget_bytes,
            model,
            pretty,
        } => {
            let root = corpus::canonical_root(&root)?;
            let mode = match mode {
                Mode::Text => "text",
                Mode::Symbol => "symbol",
                Mode::Ranked => "ranked",
                Mode::Hybrid => "hybrid",
            }
            .to_owned();
            let result = search::search(
                &root,
                &search::SearchOptions {
                    query,
                    mode,
                    glob,
                    hidden,
                    literal,
                    case_sensitive,
                    context,
                    limit,
                    budget_bytes,
                    model,
                },
            )?;
            if pretty {
                for hit in &result.results {
                    println!(
                        "{}:{}-{} {} [score {:.3}]",
                        hit.chunk.path,
                        hit.chunk.start_line,
                        hit.chunk.end_line,
                        hit.chunk.symbol.as_deref().unwrap_or(&hit.chunk.kind),
                        hit.score
                    );
                    println!("{}", hit.chunk.content);
                    if hit.excerpt_truncated {
                        println!("[excerpt clipped; read full range with agx read]");
                    }
                }
                println!(
                    "{} of {} units; truncated={}",
                    result.returned_units, result.matched_units, result.truncated
                );
                for warning in result.warnings {
                    eprintln!("warning: {warning}");
                }
            } else {
                println!("{}", serde_json::to_string(&result)?);
            }
        }
        Commands::Index { root } => {
            let root = corpus::canonical_root(&root)?;
            let (chunks, warnings) = index::refresh(&root, &[], false)?;
            println!(
                "{}",
                serde_json::json!({"root":root,"chunks":chunks.len(),"cache":index::cache_dir(&root)?,"warnings":warnings})
            );
        }
        Commands::Map { root } => println!("{}", mcp::map(&corpus::canonical_root(&root)?)?),
        Commands::Read {
            path,
            root,
            start,
            end,
            budget_bytes,
        } => println!(
            "{}",
            mcp::read_source(
                &corpus::canonical_root(&root)?,
                &path,
                start,
                end,
                budget_bytes
            )?
        ),
        Commands::Mcp { root } => mcp::serve(&corpus::canonical_root(&root)?)?,
        Commands::Skill {
            harness,
            global,
            root,
            force,
        } => install_skill(harness, global, root, force)?,
        Commands::Parse { input, out } => parse_document(input, out)?,
        Commands::Doctor => {
            let lit = Command::new("lit")
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success());
            let ollama = Command::new("ollama")
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success());
            println!(
                "{}",
                serde_json::json!({"version":env!("CARGO_PKG_VERSION"),"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"exact_search":"embedded Rust regex engine","structural_languages":["Rust","Python","TypeScript/TSX","JavaScript/JSX","Go","Swift"],"liteparse_cli":lit,"ollama_cli":ollama,"note":"Ollama CLI presence does not establish server readiness or model availability"})
            );
        }
        Commands::Clean { root } => {
            let path = index::cache_dir(&corpus::canonical_root(&root)?)?;
            std::fs::remove_dir_all(&path)?;
            println!("{}", serde_json::json!({"removed":path}));
        }
        Commands::Update { action } => println!("{}", update::run(action)?),
        Commands::UpdatePre { check } => println!("{}", update::development(check)?),
    }
    std::io::stdout().flush()?;
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        if error
            .downcast_ref::<std::io::Error>()
            .is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe)
        {
            return;
        }
        eprintln!("{}", serde_json::json!({"error":format!("{error:#}")}));
        std::process::exit(2);
    }
}
