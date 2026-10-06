//! Local editor protocol. No model/network/process APIs are reachable from this module.
use crate::{
    corpus, index,
    model::{Chunk, Evidence, bounded_response},
    search::{self, SearchOptions},
    structure,
};
use anyhow::{Context, Result};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use ignore::{WalkBuilder, gitignore::GitignoreBuilder};
use regex::RegexBuilder;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs::{self, File},
    io::{self, BufRead, Read, Write},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::{SystemTime, UNIX_EPOCH},
};

const FRAME_BYTES: usize = 4 * 1024 * 1024;
const QUEUE: usize = 32;
const QUEUE_BYTES: usize = 8 * 1024 * 1024;
const SAMPLE_LIMIT: usize = 64;
type Output = Arc<Mutex<io::Stdout>>;
fn emit(out: &Output, value: Value) -> Result<()> {
    let mut out = out.lock().unwrap();
    serde_json::to_writer(&mut *out, &value)?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}
fn error(out: &Output, id: Value, code: &str, message: &str) -> Result<()> {
    let message: String = message.chars().take(1024).collect();
    emit(
        out,
        json!({"protocol_version":1,"id":id,"error":{"code":code,"message":message}}),
    )
}
fn check(cancel: &AtomicBool) -> Result<()> {
    anyhow::ensure!(!cancel.load(Ordering::Relaxed), "cancelled");
    Ok(())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}
struct Work {
    frame_bytes: usize,
    request: Request,
    cancel: Arc<AtomicBool>,
}
fn key(id: &Value) -> Option<String> {
    match id {
        Value::String(s) if !s.is_empty() && s.len() <= 128 => Some(format!("s:{s}")),
        Value::Number(n) if n.is_u64() => Some(format!("n:{n}")),
        _ => None,
    }
}

#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Limits {
    memory_bytes: usize,
    max_file_bytes: usize,
    max_files: usize,
    response_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            memory_bytes: 64 * 1024 * 1024,
            max_file_bytes: 256 * 1024,
            max_files: 10000,
            response_bytes: 2 * 1024 * 1024,
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RootConfig {
    id: String,
    path: PathBuf,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Initialize {
    protocol_version: u32,
    workspace_id: String,
    roots: Vec<RootConfig>,
    #[serde(default = "yes")]
    restricted: bool,
    #[serde(default)]
    limits: Limits,
}
fn yes() -> bool {
    true
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RootRequest {
    root_id: String,
    #[serde(default)]
    expected_index_version: Option<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DocumentRequest {
    root_id: String,
    path: String,
    version: u64,
    content: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseRequest {
    root_id: String,
    path: String,
    version: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FilesRequest {
    root_id: String,
    paths: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Query {
    root_id: String,
    #[serde(default)]
    expected_index_version: Option<u64>,
    #[serde(default)]
    languages: Vec<String>,
    #[serde(flatten)]
    options: WorkerOptions,
}
// Separate DTO deliberately omits every model/hybrid parameter.
#[derive(Deserialize)]
#[serde(default)]
struct WorkerOptions {
    query: String,
    mode: String,
    glob: Vec<String>,
    literal: bool,
    case_sensitive: bool,
    context: usize,
    limit: usize,
    budget_bytes: usize,
}
impl Default for WorkerOptions {
    fn default() -> Self {
        Self {
            query: String::new(),
            mode: "text".into(),
            glob: vec![],
            literal: false,
            case_sensitive: false,
            context: 2,
            limit: 20,
            budget_bytes: 16000,
        }
    }
}
impl WorkerOptions {
    fn search_options(&self) -> SearchOptions {
        SearchOptions {
            query: self.query.clone(),
            mode: self.mode.clone(),
            glob: self.glob.clone(),
            literal: self.literal,
            case_sensitive: self.case_sensitive,
            context: self.context,
            limit: self.limit,
            budget_bytes: self.budget_bytes,
            ..SearchOptions::default()
        }
    }
}
#[derive(Clone, Default)]
struct Skipped {
    counts: BTreeMap<String, usize>,
    samples: Vec<Value>,
}
impl Skipped {
    fn add(&mut self, path: &str, reason: &str) {
        *self.counts.entry(reason.into()).or_default() += 1;
        if self.samples.len() < SAMPLE_LIMIT {
            self.samples.push(json!({"path":path,"reason":reason}));
        }
    }
    fn value(&self) -> Value {
        let total = self.counts.values().sum::<usize>();
        json!({"total":total,"counts":self.counts,"samples":self.samples,"samples_truncated":total>self.samples.len()})
    }
    fn incomplete(&self) -> bool {
        self.counts.keys().any(|k| {
            matches!(
                k.as_str(),
                "file_size"
                    | "memory_limit"
                    | "file_limit"
                    | "read_error"
                    | "parse_limit"
                    | "changed_during_read"
                    | "depth_limit"
            )
        })
    }
}
#[derive(Clone, PartialEq)]
struct Stamp {
    length: u64,
    modified: Option<SystemTime>,
}
fn stamp(path: &Path) -> Result<Stamp> {
    let m = fs::symlink_metadata(path)?;
    anyhow::ensure!(m.is_file(), "not a regular file");
    Ok(Stamp {
        length: m.len(),
        modified: m.modified().ok(),
    })
}
struct RankUnit {
    chunk: Arc<Chunk>,
    tf: HashMap<String, usize>,
    symbol: HashSet<String>,
    path: HashSet<String>,
    length: usize,
}
struct Document {
    content: String,
    hash: String,
    version: Option<u64>,
    stamp: Option<Stamp>,
    symbols: Vec<Arc<Chunk>>,
    units: Vec<RankUnit>,
    offsets: Vec<usize>,
    memory: usize,
    parse_limited: bool,
}
impl Document {
    fn new(
        path: &str,
        content: String,
        version: Option<u64>,
        stamp: Option<Stamp>,
        limits: &Limits,
        cancel: &AtomicBool,
    ) -> Result<Self> {
        check(cancel)?;
        let source = corpus::Source {
            path: path.into(),
            content,
        };
        let (symbols, parse_limited) =
            structure::bounded_symbols(&source, cancel, limits.max_file_bytes * 2)?;
        let symbols: Vec<_> = symbols.into_iter().map(Arc::new).collect();
        let mut chunks = symbols.clone();
        let lines: Vec<_> = source.content.lines().collect();
        for start in (0..lines.len()).step_by(60) {
            check(cancel)?;
            let end = (start + 80).min(lines.len());
            chunks.push(Arc::new(Chunk {
                path: path.into(),
                start_line: start + 1,
                end_line: end,
                symbol: None,
                kind: "window".into(),
                content: lines[start..end].join("\n"),
            }));
        }
        let mut units = Vec::new();
        let mut memory = source.content.capacity() + path.len() + 256;
        for chunk in chunks {
            check(cancel)?;
            let terms = index::terms(&chunk.content);
            let length = terms.len();
            let mut tf = HashMap::new();
            for term in terms {
                *tf.entry(term).or_insert(0) += 1;
            }
            let symbol = index::terms(chunk.symbol.as_deref().unwrap_or(""))
                .into_iter()
                .collect::<HashSet<_>>();
            let path = index::terms(path).into_iter().collect::<HashSet<_>>();
            // Conservative accounting includes allocated hash buckets and duplicated strings.
            memory += chunk.content.capacity()
                + chunk.path.capacity()
                + chunk.kind.capacity()
                + chunk.symbol.as_ref().map_or(0, String::capacity)
                + 256;
            memory += tf.capacity() * 192 + tf.keys().map(|s| s.capacity() * 2).sum::<usize>();
            memory += (symbol.capacity() + path.capacity()) * 96
                + symbol
                    .iter()
                    .chain(&path)
                    .map(String::capacity)
                    .sum::<usize>();
            units.push(RankUnit {
                chunk,
                tf,
                symbol,
                path,
                length,
            });
        }
        let mut offsets = vec![0];
        offsets.extend(source.content.match_indices('\n').map(|(i, _)| i + 1));
        if offsets.last() == Some(&source.content.len()) {
            offsets.pop();
        }
        memory += offsets.capacity() * std::mem::size_of::<usize>()
            + symbols.capacity() * 16
            + units.capacity() * 128;
        Ok(Self {
            hash: blake3::hash(source.content.as_bytes()).to_hex().to_string(),
            content: source.content,
            version,
            stamp,
            symbols,
            units,
            offsets,
            memory,
            parse_limited,
        })
    }
    fn line(&self, n: usize) -> &str {
        let start = self.offsets[n];
        let end = self
            .offsets
            .get(n + 1)
            .copied()
            .unwrap_or(self.content.len());
        self.content[start..end]
            .strip_suffix('\n')
            .map(|s| s.strip_suffix('\r').unwrap_or(s))
            .unwrap_or(&self.content[start..end])
    }
}
struct Overlay {
    content: String,
    version: u64,
}
struct Root {
    id: String,
    path: PathBuf,
    documents: BTreeMap<String, Document>,
    overlays: BTreeMap<String, Overlay>,
    versions: HashMap<String, u64>,
    generation: u64,
    ready: bool,
    dirty: bool,
    skipped: Skipped,
    dfs: HashMap<String, usize>,
    unit_count: usize,
    total_terms: usize,
    cached_bytes: usize,
    df_text_bytes: usize,
}
impl Root {
    fn remove(&mut self, path: &str) {
        if let Some(doc) = self.documents.remove(path) {
            self.cached_bytes -= doc.memory;
            for unit in doc.units {
                self.unit_count -= 1;
                self.total_terms -= unit.length;
                for term in unit.tf.keys() {
                    if let Some(df) = self.dfs.get_mut(term) {
                        *df -= 1;
                        if *df == 0 {
                            self.df_text_bytes -= term.len();
                            self.dfs.remove(term);
                        }
                    }
                }
            }
        }
        if self.dfs.len() * 4 < self.dfs.capacity() {
            self.dfs.shrink_to_fit();
        }
    }
    fn insert(&mut self, path: String, doc: Document) {
        self.remove(&path);
        self.cached_bytes += doc.memory;
        for unit in &doc.units {
            self.unit_count += 1;
            self.total_terms += unit.length;
            for term in unit.tf.keys() {
                if !self.dfs.contains_key(term) {
                    self.df_text_bytes += term.len();
                }
                *self.dfs.entry(term.clone()).or_default() += 1;
            }
        }
        self.documents.insert(path, doc);
    }
    fn bytes(&self) -> usize {
        self.cached_bytes
            + self.dfs.capacity() * 96
            + self.df_text_bytes
            + self
                .overlays
                .iter()
                .map(|(p, o)| p.capacity() + o.content.capacity() + 128)
                .sum::<usize>()
            + self.versions.capacity() * 96
            + self.versions.keys().map(String::capacity).sum::<usize>()
    }
}
struct State {
    workspace: String,
    session: String,
    roots: BTreeMap<String, Root>,
    limits: Limits,
}
fn excluded(path: &Path) -> bool {
    path.components().any(|c|matches!(c,Component::Normal(n) if matches!(n.to_str(),Some(".git"|"node_modules"|"target"|".venv"|"vendor"|"dist"|"build"|".agentgrep"))))
}
fn confined(root: &Path, relative: &str) -> Result<PathBuf> {
    let p = Path::new(relative);
    anyhow::ensure!(
        relative.len() <= 4096
            && !relative.is_empty()
            && !p.is_absolute()
            && p.components().all(|c| matches!(c, Component::Normal(_))),
        "path must be a relative normal path without traversal"
    );
    let mut path = root.to_path_buf();
    for part in p.components() {
        path.push(part);
        match fs::symlink_metadata(&path) {
            Ok(m) => anyhow::ensure!(!m.file_type().is_symlink(), "symlink paths are not allowed"),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    // Check existing parent confinement again against canonical paths.
    let mut ancestor = path.as_path();
    while !ancestor.exists() {
        ancestor = ancestor.parent().context("missing root")?;
    }
    anyhow::ensure!(
        ancestor.canonicalize()?.starts_with(root),
        "path escaped workspace root"
    );
    Ok(path)
}
fn eligible(root: &Path, relative: &str) -> Result<bool> {
    eligible_entry(root, relative, false)
}
fn eligible_entry(root: &Path, relative: &str, is_dir: bool) -> Result<bool> {
    let path = Path::new(relative);
    if excluded(path)
        || path
            .components()
            .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
        || (!is_dir && corpus::is_binary_format(path))
    {
        return Ok(false);
    }
    let mut git = GitignoreBuilder::new(root);
    let mut local = GitignoreBuilder::new(root);
    let mut dir = root.to_path_buf();
    let parts = path.components().collect::<Vec<_>>();
    for (i, part) in parts.iter().enumerate() {
        for (name, builder) in [(".gitignore", &mut git), (".ignore", &mut local)] {
            let rule = dir.join(name);
            if fs::symlink_metadata(&rule).is_ok() {
                let relative = rule
                    .strip_prefix(root)?
                    .to_str()
                    .context("non-UTF8 ignore path")?;
                let (content, _) = read_file(root, relative, 64 * 1024)
                    .context("ignore rule must be a confined UTF-8 regular file <=64 KiB")?;
                for line in content.lines() {
                    builder.add_line(Some(rule.clone()), line)?;
                }
            }
        }
        dir.push(part);
        let directory = i + 1 < parts.len() || is_dir;
        let local_matcher = local.build()?;
        let local = local_matcher.matched(&dir, directory);
        let ignored = if local.is_none() {
            git.build()?.matched(&dir, directory).is_ignore()
        } else {
            local.is_ignore()
        };
        if ignored {
            return Ok(false);
        }
    }
    Ok(true)
}
fn read_file(root: &Path, path: &str, max: usize) -> Result<(String, Stamp)> {
    let full = confined(root, path)?;
    let before = stamp(&full)?;
    anyhow::ensure!(before.length <= max as u64, "file_size");
    let mut bytes = Vec::new();
    File::open(&full)?
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= max, "file_size");
    anyhow::ensure!(!bytes.contains(&0), "binary");
    let text = String::from_utf8(bytes).context("non_utf8")?;
    anyhow::ensure!(stamp(&full)? == before, "changed_during_read");
    Ok((text, before))
}
fn globs(patterns: &[String]) -> Result<(GlobSet, GlobSet, bool)> {
    let (mut includes, mut excludes) = (GlobSetBuilder::new(), GlobSetBuilder::new());
    let mut any = false;
    for s in patterns {
        let (exclude, p) = s
            .strip_prefix('!')
            .map_or((false, s.as_str()), |s| (true, s));
        let p = if p.contains('/') {
            p.to_owned()
        } else {
            format!("**/{p}")
        };
        let g = GlobBuilder::new(&p).literal_separator(true).build()?;
        if exclude {
            excludes.add(g);
        } else {
            any = true;
            includes.add(g);
        }
    }
    Ok((includes.build()?, excludes.build()?, any))
}
fn language(path: &str) -> &str {
    match Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
    {
        "rs" => "rust",
        "py" => "python",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "go" => "go",
        "swift" => "swift",
        _ => "text",
    }
}
impl State {
    fn initialize(params: Value) -> Result<Self> {
        let p: Initialize = serde_json::from_value(params)?;
        anyhow::ensure!(p.protocol_version == 1, "unsupported_protocol");
        anyhow::ensure!(
            p.restricted,
            "restricted worker cannot expose model operations"
        );
        anyhow::ensure!(
            !p.workspace_id.is_empty() && p.workspace_id.len() <= 128,
            "invalid workspace_id"
        );
        anyhow::ensure!(
            !p.roots.is_empty() && p.roots.len() <= 8,
            "configure 1..8 roots"
        );
        anyhow::ensure!(
            (1024 * 1024..=256 * 1024 * 1024).contains(&p.limits.memory_bytes),
            "memory_bytes must be 1..256 MiB"
        );
        anyhow::ensure!(
            (1024..=corpus::MAX_FILE_BYTES as usize).contains(&p.limits.max_file_bytes),
            "max_file_bytes must be 1 KiB..2 MiB"
        );
        anyhow::ensure!(
            (1..=100000).contains(&p.limits.max_files),
            "max_files must be 1..100000"
        );
        anyhow::ensure!(
            (4096..=FRAME_BYTES).contains(&p.limits.response_bytes),
            "response_bytes must be 4 KiB..4 MiB"
        );
        let mut roots = BTreeMap::new();
        let mut paths = HashSet::new();
        for config in p.roots {
            anyhow::ensure!(
                !config.id.is_empty() && config.id.len() <= 128 && !roots.contains_key(&config.id),
                "duplicate/invalid root id"
            );
            let path = corpus::canonical_root(&config.path)?;
            anyhow::ensure!(paths.insert(path.clone()), "duplicate canonical root");
            roots.insert(
                config.id.clone(),
                Root {
                    id: config.id,
                    path,
                    documents: BTreeMap::new(),
                    overlays: BTreeMap::new(),
                    versions: HashMap::new(),
                    generation: 0,
                    ready: false,
                    dirty: false,
                    skipped: Skipped::default(),
                    dfs: HashMap::new(),
                    unit_count: 0,
                    total_terms: 0,
                    cached_bytes: 0,
                    df_text_bytes: 0,
                },
            );
        }
        Ok(Self {
            workspace: p.workspace_id,
            session: format!(
                "{}-{}",
                std::process::id(),
                SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
            ),
            roots,
            limits: p.limits,
        })
    }
    fn info(&self) -> Value {
        json!({"protocol_version":1,"result_schema_version":2,"workspace_id":self.workspace,"session_id":self.session,"restricted":true,"search_modes":["text","symbol","ranked"],"ranked_algorithm":"lexical BM25; no embeddings or model inference","capabilities":{"cancellation":true,"document_overrides":true,"document_versions":true,"document_save":true,"file_notifications":true,"index_progress":true,"language_filters":true,"network":false,"telemetry":false,"hybrid":false,"models":false},"limits":{"memory_bytes":self.limits.memory_bytes,"max_file_bytes":self.limits.max_file_bytes,"max_files":self.limits.max_files,"response_bytes":self.limits.response_bytes,"request_bytes":FRAME_BYTES,"queue_requests":QUEUE,"queue_bytes":QUEUE_BYTES,"result_limit":1000,"source_budget_bytes":1000000},"roots":self.roots.values().map(|r|json!({"id":r.id,"path":r.path,"index_version":r.generation})).collect::<Vec<_>>()})
    }
    fn memory(&self) -> usize {
        self.roots.values().map(Root::bytes).sum()
    }
    fn root(&self, id: &str) -> Result<&Root> {
        self.roots.get(id).context("unknown_root")
    }
    fn progress(
        &self,
        out: &Output,
        id: &Value,
        root: &str,
        phase: &str,
        counts: (usize, usize, usize),
    ) -> Result<()> {
        let (seen, read, reused) = counts;
        emit(
            out,
            json!({"protocol_version":1,"method":"index/progress","params":{"request_id":id,"workspace_id":self.workspace,"root_id":root,"phase":phase,"session_id":self.session,"index_version":self.root(root)?.generation,"files_seen":seen,"files_read":read,"files_reused":reused}}),
        )
    }
    fn load_path(
        &mut self,
        root_id: &str,
        path: &str,
        force: bool,
        cancel: &AtomicBool,
    ) -> Result<(bool, bool)> {
        check(cancel)?;
        let root = self.root(root_id)?;
        let full = confined(&root.path, path)?;
        if !eligible(&root.path, path)? {
            self.roots.get_mut(root_id).unwrap().remove(path);
            return Ok((false, false));
        }
        let old = root.documents.get(path);
        let overlay = root.overlays.get(path);
        let disk_stamp = if overlay.is_none() {
            stamp(&full).ok()
        } else {
            None
        };
        if !force
            && old.is_some_and(|d| {
                d.version == overlay.map(|o| o.version)
                    && (overlay.is_some() || d.stamp == disk_stamp)
            })
        {
            if old.is_some_and(|d| d.parse_limited) {
                self.roots
                    .get_mut(root_id)
                    .unwrap()
                    .skipped
                    .add(path, "parse_limit");
            }
            return Ok((false, true));
        }
        let version = overlay.map(|o| o.version);
        let loaded = if let Some(o) = overlay {
            Ok((o.content.clone(), None))
        } else {
            read_file(&root.path, path, self.limits.max_file_bytes).map(|(c, s)| (c, Some(s)))
        };
        let (content, stamp) = match loaded {
            Ok(x) => x,
            Err(e) => {
                let message = e.to_string();
                let reason = if matches!(
                    message.as_str(),
                    "file_size" | "binary" | "non_utf8" | "changed_during_read" | "depth_limit"
                ) {
                    message.as_str()
                } else {
                    "read_error"
                };
                let root = self.roots.get_mut(root_id).unwrap();
                root.remove(path);
                root.skipped.add(path, reason);
                return Ok((true, false));
            }
        };
        if old.is_some_and(|d| d.hash == blake3::hash(content.as_bytes()).to_hex().as_str()) {
            let doc = self
                .roots
                .get_mut(root_id)
                .unwrap()
                .documents
                .get_mut(path)
                .unwrap();
            doc.version = version;
            doc.stamp = stamp;
            if doc.parse_limited {
                self.roots
                    .get_mut(root_id)
                    .unwrap()
                    .skipped
                    .add(path, "parse_limit");
            }
            return Ok((version.is_none(), true));
        }
        let doc = Document::new(path, content, version, stamp, &self.limits, cancel)?;
        let old_memory = old.map_or(0, |d| d.memory);
        if self.memory().saturating_sub(old_memory)
            + doc.memory * 2
            + self.limits.max_file_bytes * 2
            > self.limits.memory_bytes
            || (old.is_none()
                && self
                    .roots
                    .values()
                    .map(|r| r.documents.len())
                    .sum::<usize>()
                    >= self.limits.max_files)
        {
            let reason = if old.is_none()
                && self
                    .roots
                    .values()
                    .map(|r| r.documents.len())
                    .sum::<usize>()
                    >= self.limits.max_files
            {
                "file_limit"
            } else {
                "memory_limit"
            };
            let root = self.roots.get_mut(root_id).unwrap();
            root.remove(path);
            root.skipped.add(path, reason);
            return Ok((true, false));
        }
        let root = self.roots.get_mut(root_id).unwrap();
        if doc.parse_limited {
            root.skipped.add(path, "parse_limit");
        }
        root.insert(path.into(), doc);
        Ok((version.is_none(), false))
    }
    fn refresh(
        &mut self,
        id: &Value,
        root_id: &str,
        force: bool,
        cancel: &AtomicBool,
        out: &Output,
    ) -> Result<Value> {
        let root = self.roots.get_mut(root_id).context("unknown_root")?;
        root.generation += 1;
        root.dirty = true;
        root.skipped = Skipped::default();
        let path = root.path.clone();
        self.progress(out, id, root_id, "started", (0, 0, 0))?;
        // Metadata-only traversal; cached contents/parses are reused unless stamps changed.
        let mut walker = WalkBuilder::new(&path);
        walker
            .hidden(true)
            .follow_links(false)
            .require_git(false)
            .parents(false)
            .git_global(false)
            .git_exclude(false)
            .git_ignore(false)
            .ignore(false)
            .max_depth(Some(64));
        let base = path.clone();
        let discovery_errors = Arc::new(Mutex::new(Vec::new()));
        let callback_errors = discovery_errors.clone();
        walker.filter_entry(move |e| {
            let Ok(p) = e.path().strip_prefix(&base) else {
                return false;
            };
            if p.as_os_str().is_empty() {
                return true;
            }
            if excluded(p) {
                return false;
            }
            if e.file_type().is_some_and(|f| f.is_dir()) {
                match eligible_entry(&base, &p.to_string_lossy(), true) {
                    Ok(allowed) => allowed,
                    Err(error) => {
                        let mut errors = callback_errors.lock().unwrap();
                        if errors.len() < SAMPLE_LIMIT {
                            errors.push(error.to_string());
                        }
                        false
                    }
                }
            } else {
                true
            }
        });
        let mut live = BTreeSet::new();
        let mut live_bytes = 0;
        let mut seen = 0;
        let mut read = 0;
        let mut reused = 0;
        for entry in walker.build() {
            check(cancel)?;
            let entry = match entry {
                Ok(e) => e,
                Err(_) => {
                    self.roots
                        .get_mut(root_id)
                        .unwrap()
                        .skipped
                        .add("", "read_error");
                    continue;
                }
            };
            if entry.path() == path {
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(&path)?
                .to_str()
                .context("non-UTF8 path")?
                .to_owned();
            if entry.file_type().is_some_and(|f| f.is_symlink()) {
                self.roots
                    .get_mut(root_id)
                    .unwrap()
                    .skipped
                    .add(&relative, "symlink");
                continue;
            }
            if !entry.file_type().is_some_and(|f| f.is_file()) {
                if entry.depth() >= 64 {
                    self.roots
                        .get_mut(root_id)
                        .unwrap()
                        .skipped
                        .add(&relative, "depth_limit");
                }
                continue;
            }
            if corpus::is_binary_format(entry.path()) {
                self.roots
                    .get_mut(root_id)
                    .unwrap()
                    .skipped
                    .add(&relative, "binary_format");
                continue;
            }
            if !eligible(&path, &relative)? {
                continue;
            }
            if seen >= self.limits.max_files || live_bytes > self.limits.memory_bytes / 8 {
                self.roots
                    .get_mut(root_id)
                    .unwrap()
                    .skipped
                    .add(&relative, "file_limit");
                break;
            }
            live_bytes += relative.len() + 64;
            live.insert(relative.clone());
            seen += 1;
            let (r, u) = self.load_path(root_id, &relative, force, cancel)?;
            read += usize::from(r);
            reused += usize::from(u);
            if seen % 32 == 0 {
                self.progress(out, id, root_id, "indexing", (seen, read, reused))?;
            }
        }
        for _ in discovery_errors.lock().unwrap().iter() {
            self.roots
                .get_mut(root_id)
                .unwrap()
                .skipped
                .add("", "read_error");
        }
        let overlay_paths = self
            .root(root_id)?
            .overlays
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for relative in overlay_paths {
            check(cancel)?;
            if live.contains(&relative) {
                continue;
            }
            if eligible(&path, &relative)? {
                live.insert(relative.clone());
                seen += 1;
                let (r, u) = self.load_path(root_id, &relative, false, cancel)?;
                read += usize::from(r);
                reused += usize::from(u);
            }
        }
        let removed = self
            .root(root_id)?
            .documents
            .keys()
            .filter(|p| !live.contains(*p))
            .cloned()
            .collect::<Vec<_>>();
        for p in removed {
            check(cancel)?;
            self.roots.get_mut(root_id).unwrap().remove(&p);
        }
        let root = self.roots.get_mut(root_id).unwrap();
        root.ready = true;
        root.dirty = false;
        self.progress(out, id, root_id, "complete", (seen, read, reused))?;
        Ok(self.status(root_id, read, reused))
    }
    fn status(&self, root_id: &str, read: usize, reused: usize) -> Value {
        let r = &self.roots[root_id];
        json!({"workspace_id":self.workspace,"session_id":self.session,"root_id":root_id,"index_version":r.generation,"ready":r.ready,"incomplete":r.dirty||r.skipped.incomplete(),"files_indexed":r.documents.len(),"files_read":read,"files_reused":reused,"memory_accounted_bytes":self.memory(),"skipped_files":r.skipped.value()})
    }
    fn expected(&self, root_id: &str, expected: Option<u64>) -> Result<()> {
        let r = self.root(root_id)?;
        anyhow::ensure!(expected.is_none_or(|v| v == r.generation), "stale_index");
        Ok(())
    }
    fn document(&mut self, p: DocumentRequest, cancel: &AtomicBool) -> Result<Value> {
        let root = self.root(&p.root_id)?;
        confined(&root.path, &p.path)?;
        anyhow::ensure!(
            p.content.len() <= self.limits.max_file_bytes,
            "document exceeds max_file_bytes"
        );
        anyhow::ensure!(!p.content.contains('\0'), "binary document");
        anyhow::ensure!(
            root.versions.get(&p.path).is_none_or(|v| p.version > *v),
            "stale_document"
        );
        anyhow::ensure!(
            root.versions.contains_key(&p.path) || root.versions.len() < self.limits.max_files,
            "document version table full"
        );
        let old_overlay = root.overlays.get(&p.path).map_or(0, |o| o.content.len());
        anyhow::ensure!(
            self.memory().saturating_sub(old_overlay)
                + p.content.len()
                + self.limits.max_file_bytes * 2
                < self.limits.memory_bytes,
            "document exceeds worker memory budget"
        );
        // Prepare the entire replacement before mutating document/version state.
        let doc = if eligible(&root.path, &p.path)? {
            Some(Document::new(
                &p.path,
                p.content.clone(),
                Some(p.version),
                None,
                &self.limits,
                cancel,
            )?)
        } else {
            None
        };
        let old_doc = root.documents.get(&p.path).map_or(0, |d| d.memory);
        anyhow::ensure!(
            self.memory().saturating_sub(old_overlay + old_doc)
                + p.content.len()
                + doc.as_ref().map_or(0, |d| d.memory * 2)
                + p.path.len() * 4
                + self.limits.max_file_bytes * 2
                < self.limits.memory_bytes,
            "document exceeds worker memory budget"
        );
        anyhow::ensure!(
            root.documents.contains_key(&p.path)
                || self
                    .roots
                    .values()
                    .map(|r| r.documents.len())
                    .sum::<usize>()
                    < self.limits.max_files,
            "file limit reached"
        );
        check(cancel)?;
        let root = self.roots.get_mut(&p.root_id).unwrap();
        root.generation += 1;
        root.versions.insert(p.path.clone(), p.version);
        root.overlays.insert(
            p.path.clone(),
            Overlay {
                content: p.content,
                version: p.version,
            },
        );
        if let Some(doc) = doc {
            if doc.parse_limited {
                root.skipped.add(&p.path, "parse_limit");
            }
            root.insert(p.path.clone(), doc);
        } else {
            root.remove(&p.path);
        }
        Ok(
            json!({"root_id":p.root_id,"path":p.path,"document_version":p.version,"index_version":root.generation,"indexed":root.documents.contains_key(&p.path)}),
        )
    }
    fn close(&mut self, p: CloseRequest, cancel: &AtomicBool) -> Result<Value> {
        let root = self.root(&p.root_id)?;
        confined(&root.path, &p.path)?;
        anyhow::ensure!(
            root.overlays
                .get(&p.path)
                .is_some_and(|o| o.version == p.version),
            "stale_document"
        );
        check(cancel)?;
        let root = self.roots.get_mut(&p.root_id).unwrap();
        let was_dirty = root.dirty;
        root.generation += 1;
        root.dirty = true;
        root.overlays.remove(&p.path);
        if root.path.join(&p.path).exists() {
            self.load_path(&p.root_id, &p.path, true, cancel)?;
        } else {
            self.roots.get_mut(&p.root_id).unwrap().remove(&p.path);
        }
        self.roots.get_mut(&p.root_id).unwrap().dirty = was_dirty;
        Ok(self.status(&p.root_id, 1, 0))
    }
    fn files(&mut self, p: FilesRequest, cancel: &AtomicBool) -> Result<Value> {
        anyhow::ensure!(p.paths.len() <= 1024, "notify at most 1024 paths");
        let root = self.root(&p.root_id)?;
        for path in &p.paths {
            confined(&root.path, path)?;
        }
        let root = self.roots.get_mut(&p.root_id).unwrap();
        let was_dirty = root.dirty;
        root.generation += 1;
        root.dirty = true;
        let mut read = 0;
        let mut reused = 0;
        for path in p.paths {
            check(cancel)?;
            if !self.root(&p.root_id)?.overlays.contains_key(&path)
                && !self.root(&p.root_id)?.path.join(&path).exists()
            {
                self.roots.get_mut(&p.root_id).unwrap().remove(&path);
                continue;
            }
            let (r, u) = self.load_path(&p.root_id, &path, true, cancel)?;
            read += usize::from(r);
            reused += usize::from(u);
        }
        self.roots.get_mut(&p.root_id).unwrap().dirty = was_dirty;
        Ok(self.status(&p.root_id, read, reused))
    }
}

#[derive(Clone)]
struct Hit {
    path: String,
    start: usize,
    end: usize,
    symbol: Option<String>,
    kind: String,
    score: f64,
    matches: Vec<usize>,
    match_truncated: bool,
}
fn compare(a: &Hit, b: &Hit) -> std::cmp::Ordering {
    b.score
        .total_cmp(&a.score)
        .then(a.path.cmp(&b.path))
        .then(a.start.cmp(&b.start))
        .then(a.end.cmp(&b.end))
}
fn retain(hits: &mut Vec<Hit>, hit: Hit, limit: usize) {
    let pos = hits
        .binary_search_by(|h| compare(h, &hit))
        .unwrap_or_else(|p| p);
    if pos < limit {
        hits.insert(pos, hit);
        hits.truncate(limit);
    }
}
impl State {
    fn query(&self, p: Query, cancel: &AtomicBool) -> Result<Value> {
        self.expected(&p.root_id, p.expected_index_version)?;
        let root = self.root(&p.root_id)?;
        anyhow::ensure!(root.ready && !root.dirty, "index_not_ready");
        let options = p.options.search_options();
        search::validate(&options)?;
        anyhow::ensure!(options.query.len() <= 4096, "query must be <=4096 bytes");
        anyhow::ensure!(
            options.glob.len() <= 64 && options.glob.iter().all(|g| g.len() <= 4096),
            "too many/large glob patterns"
        );
        anyhow::ensure!(
            matches!(options.mode.as_str(), "text" | "symbol" | "ranked"),
            "unsupported_mode"
        );
        let (includes, excludes, any) = globs(&options.glob)?;
        anyhow::ensure!(
            p.languages.len() <= 16
                && p.languages.iter().all(|l| [
                    "rust",
                    "python",
                    "typescript",
                    "javascript",
                    "go",
                    "swift",
                    "text"
                ]
                .contains(&l.as_str())),
            "unsupported language filter"
        );
        let query_terms = index::terms(&options.query)
            .into_iter()
            .collect::<BTreeSet<_>>();
        let pattern = if options.literal {
            regex::escape(&options.query)
        } else {
            options.query.clone()
        };
        let regex = if options.mode == "ranked" {
            None
        } else {
            Some(
                RegexBuilder::new(&pattern)
                    .case_insensitive(
                        !options.case_sensitive && !options.query.chars().any(char::is_uppercase),
                    )
                    .build()?,
            )
        };
        let mut hits = Vec::new();
        let mut matched = 0;
        for (path, doc) in &root.documents {
            check(cancel)?;
            if (any && !includes.is_match(path))
                || excludes.is_match(path)
                || (!p.languages.is_empty() && !p.languages.iter().any(|l| l == language(path)))
            {
                continue;
            }
            if options.mode == "ranked" {
                // Statistics are cached incrementally for the whole root. Filters constrain candidates.
                let average = (root.total_terms as f64 / root.unit_count.max(1) as f64).max(1.0);
                let mut accepted: Vec<(usize, usize)> = Vec::new();
                let mut ranked = Vec::new();
                for unit in &doc.units {
                    check(cancel)?;
                    let mut score = 0.0;
                    for term in &query_terms {
                        let tf = *unit.tf.get(term).unwrap_or(&0) as f64;
                        let df = *root.dfs.get(term).unwrap_or(&0) as f64;
                        let idf = (1.0 + (root.unit_count as f64 - df + 0.5) / (df + 0.5)).ln();
                        score += idf * tf * 2.2
                            / (tf + 1.2 * (0.25 + 0.75 * unit.length as f64 / average));
                        if unit.symbol.contains(term) {
                            score += idf * 1.5;
                        }
                        if unit.path.contains(term) {
                            score += idf * 0.3;
                        }
                    }
                    if score > 0.0 {
                        ranked.push((unit, score));
                    }
                }
                ranked.sort_by(|a, b| {
                    b.1.total_cmp(&a.1)
                        .then(a.0.chunk.start_line.cmp(&b.0.chunk.start_line))
                });
                for (unit, score) in ranked {
                    check(cancel)?;
                    let c = &unit.chunk;
                    if accepted.iter().any(|(s, e)| {
                        let a = c.start_line.max(*s);
                        let b = c.end_line.min(*e);
                        b >= a && (b - a + 1) * 2 >= (c.end_line - c.start_line + 1).min(e - s + 1)
                    }) {
                        continue;
                    }
                    accepted.push((c.start_line, c.end_line));
                    matched += 1;
                    retain(
                        &mut hits,
                        Hit {
                            path: path.clone(),
                            start: c.start_line,
                            end: c.end_line,
                            symbol: c.symbol.clone(),
                            kind: c.kind.clone(),
                            score,
                            matches: vec![],
                            match_truncated: false,
                        },
                        options.limit,
                    );
                }
            } else {
                let regex = regex.as_ref().unwrap();
                let mut units: BTreeMap<(usize, usize), Hit> = BTreeMap::new();
                let mut last = None;
                for line in 0..doc.offsets.len() {
                    check(cancel)?;
                    if !regex.is_match(doc.line(line)) {
                        continue;
                    }
                    let symbol = if options.mode == "symbol" {
                        doc.symbols
                            .iter()
                            .filter(|c| c.start_line <= line + 1 && c.end_line > line)
                            .min_by_key(|c| c.end_line - c.start_line)
                    } else {
                        None
                    };
                    let (start, end) = symbol.map_or(
                        (
                            line.saturating_sub(options.context) + 1,
                            (line + 1 + options.context).min(doc.offsets.len()),
                        ),
                        |c| (c.start_line, c.end_line),
                    );
                    let hit = Hit {
                        path: path.clone(),
                        start,
                        end,
                        symbol: symbol.and_then(|c| c.symbol.clone()),
                        kind: symbol.map_or("lines".into(), |c| c.kind.clone()),
                        score: 1.0,
                        matches: vec![line + 1],
                        match_truncated: false,
                    };
                    if symbol.is_some() {
                        let h = units.entry((start, end)).or_insert_with(|| {
                            matched += 1;
                            let mut h = hit.clone();
                            h.matches.clear();
                            h
                        });
                        if h.matches.len() < 256 {
                            h.matches.push(line + 1);
                        } else {
                            h.match_truncated = true;
                        }
                    } else if last != Some((start, end)) {
                        matched += 1;
                        retain(&mut hits, hit, options.limit);
                        last = Some((start, end));
                    } else if let Some(h) = hits
                        .iter_mut()
                        .find(|h| h.path == *path && h.start == start && h.end == end)
                    {
                        if h.matches.len() < 256 {
                            h.matches.push(line + 1);
                        } else {
                            h.match_truncated = true;
                        }
                    }
                }
                for h in units.into_values() {
                    retain(&mut hits, h, options.limit);
                }
            }
        }
        check(cancel)?;
        let mut evidence = Vec::new();
        let mut remaining = options.budget_bytes;
        let mut details = BTreeMap::new();
        for h in hits {
            if remaining == 0 {
                break;
            }
            let doc = &root.documents[&h.path];
            let mut content = String::new();
            let mut clipped = false;
            for line in h.start - 1..h.end {
                check(cancel)?;
                if line > h.start - 1 {
                    if content.len() == remaining {
                        clipped = true;
                        break;
                    }
                    content.push('\n');
                }
                let text = doc.line(line);
                let take = (remaining - content.len()).min(text.len());
                let mut end = take;
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                content.push_str(&text[..end]);
                if end < text.len() {
                    clipped = true;
                    break;
                }
            }
            remaining -= content.len();
            details.insert((h.path.clone(), h.start, h.end), h.match_truncated);
            evidence.push(Evidence {
                chunk: Chunk {
                    path: h.path,
                    start_line: h.start,
                    end_line: h.end,
                    symbol: h.symbol,
                    kind: h.kind,
                    content,
                },
                score: h.score,
                match_lines: h.matches,
                excerpt_truncated: clipped,
            });
        }
        let mut response = bounded_response(
            &root.path,
            &options.mode,
            &options.query,
            evidence,
            options.limit,
            options.budget_bytes,
            vec![],
        );
        response.matched_units = matched;
        response.truncated |= response.returned_units < matched;
        let mut value = serde_json::to_value(response)?;
        value["schema_version"] = json!(2);
        value["workspace_id"] = json!(self.workspace);
        value["session_id"] = json!(self.session);
        value["root_id"] = json!(p.root_id);
        value["index_version"] = json!(root.generation);
        value["incomplete"] = json!(root.skipped.incomplete());
        if root.skipped.incomplete() {
            value["warnings"] = json!([
                "Some eligible files or syntax declarations were omitted; inspect skipped_files"
            ]);
        }
        value["skipped_files"] = root.skipped.value();
        value["response_truncated"] = json!(false);
        let results = value["results"].as_array_mut().unwrap();
        for hit in results {
            let path = hit["path"].as_str().unwrap().to_owned();
            let doc = &root.documents[&path];
            let start = hit["start_line"].as_u64().unwrap() as usize;
            let end = hit["end_line"].as_u64().unwrap() as usize;
            hit["root_id"] = json!(p.root_id);
            hit["workspace_id"] = json!(self.workspace);
            hit["document_version"] = json!(doc.version);
            hit["content_hash"] = json!(doc.hash);
            hit["index_version"] = json!(root.generation);
            hit["source"] = json!(if doc.version.is_some() {
                "overlay"
            } else {
                "disk"
            });
            hit["language"] = json!(language(&path));
            hit["source_range"] = json!({"start":{"line":start,"byte_column":0},"end":{"line":end,"byte_column":doc.line(end-1).len()},"end_exclusive":true});
            hit["match_lines_truncated"] = json!(details[&(path, start, end)]);
        }
        // Budget metadata once, then fit results individually; avoid repeatedly serializing
        // a large response for every omitted hit.
        let all = value["results"].take().as_array().unwrap().clone();
        value["results"] = json!([]);
        let mut used = serde_json::to_vec(&value)?.len() + 1024;
        if used > self.limits.response_bytes {
            value["skipped_files"]["samples"] = json!([]);
            value["skipped_files"]["samples_truncated"] = json!(true);
            used = serde_json::to_vec(&value)?.len() + 1024;
            anyhow::ensure!(used <= self.limits.response_bytes, "response_limit");
            value["response_truncated"] = json!(true);
        }
        for mut hit in all {
            check(cancel)?;
            let mut size = serde_json::to_vec(&hit)?.len() + 1;
            while used + size > self.limits.response_bytes
                && hit["content"].as_str().unwrap().len() > 256
            {
                let content = hit["content"].as_str().unwrap();
                let mut end = content.len() / 2;
                while !content.is_char_boundary(end) {
                    end -= 1;
                }
                hit["content"] = json!(&content[..end]);
                hit["excerpt_truncated"] = json!(true);
                value["response_truncated"] = json!(true);
                size = serde_json::to_vec(&hit)?.len() + 1;
            }
            if used + size > self.limits.response_bytes {
                value["response_truncated"] = json!(true);
                break;
            }
            used += size;
            value["results"].as_array_mut().unwrap().push(hit);
        }
        value["returned_units"] = json!(value["results"].as_array().unwrap().len());
        if value["response_truncated"] == true {
            value["truncated"] = json!(true);
        }
        Ok(value)
    }
}
fn classify(e: &anyhow::Error) -> &str {
    let message = e.to_string();
    for code in [
        "cancelled",
        "unknown_root",
        "stale_index",
        "stale_document",
        "index_not_ready",
        "unsupported_protocol",
        "unsupported_mode",
        "response_limit",
    ] {
        if message == code {
            return code;
        }
    }
    "invalid_params"
}
fn dispatch(state: &mut Option<State>, work: &Work, out: &Output) -> Result<Value> {
    check(&work.cancel)?;
    let r = &work.request;
    if r.method == "initialize" {
        anyhow::ensure!(state.is_none(), "already initialized");
        let new = State::initialize(r.params.clone())?;
        let info = new.info();
        *state = Some(new);
        return Ok(info);
    }
    let state = state.as_mut().context("initialize first")?;
    match r.method.as_str() {
        "capabilities" => {
            anyhow::ensure!(
                r.params.is_null() || r.params.as_object().is_some_and(|m| m.is_empty()),
                "capabilities takes no parameters"
            );
            Ok(state.info())
        }
        "index/refresh" | "index/rescan" | "workspace/ignore_changed" => {
            let p: RootRequest = serde_json::from_value(r.params.clone())?;
            state.expected(&p.root_id, p.expected_index_version)?;
            state.refresh(
                &r.id,
                &p.root_id,
                r.method == "index/rescan",
                &work.cancel,
                out,
            )
        }
        "index/status" => {
            let p: RootRequest = serde_json::from_value(r.params.clone())?;
            state.expected(&p.root_id, p.expected_index_version)?;
            Ok(state.status(&p.root_id, 0, 0))
        }
        "document/update" => {
            state.document(serde_json::from_value(r.params.clone())?, &work.cancel)
        }
        "document/close" | "document/save" => {
            state.close(serde_json::from_value(r.params.clone())?, &work.cancel)
        }
        "workspace/files_changed" => {
            state.files(serde_json::from_value(r.params.clone())?, &work.cancel)
        }
        "search" => {
            let allowed = [
                "root_id",
                "expected_index_version",
                "languages",
                "query",
                "mode",
                "glob",
                "literal",
                "case_sensitive",
                "context",
                "limit",
                "budget_bytes",
            ];
            let object = r
                .params
                .as_object()
                .context("search params must be an object")?;
            anyhow::ensure!(
                object.keys().all(|k| allowed.contains(&k.as_str())),
                "restricted search rejects unknown/model parameters"
            );
            let p: Query = serde_json::from_value(r.params.clone())?;
            anyhow::ensure!(
                matches!(p.options.mode.as_str(), "text" | "symbol" | "ranked"),
                "unsupported_mode"
            );
            state.query(p, &work.cancel)
        }
        _ => anyhow::bail!("method_not_found"),
    }
}

pub fn serve() -> Result<()> {
    let out = Arc::new(Mutex::new(io::stdout()));
    let active = Arc::new(Mutex::new(HashMap::<String, Arc<AtomicBool>>::new()));
    let (tx, rx) = mpsc::sync_channel::<Work>(QUEUE);
    let queued_bytes = Arc::new(AtomicUsize::new(0));
    let reader_bytes = queued_bytes.clone();
    let reader_out = out.clone();
    let reader_active = active.clone();
    let reader = std::thread::spawn(move || -> Result<()> {
        let stdin = io::stdin();
        let mut input = stdin.lock();
        loop {
            let mut bytes = Vec::new();
            let n = input
                .by_ref()
                .take(FRAME_BYTES as u64 + 1)
                .read_until(b'\n', &mut bytes)?;
            if n == 0 {
                break;
            }
            if bytes.len() > FRAME_BYTES {
                if bytes.last() != Some(&b'\n') {
                    loop {
                        let buf = input.fill_buf()?;
                        if buf.is_empty() {
                            break;
                        }
                        let end = buf.iter().position(|b| *b == b'\n');
                        let n = end.map_or(buf.len(), |p| p + 1);
                        input.consume(n);
                        if end.is_some() {
                            break;
                        }
                    }
                }
                error(
                    &reader_out,
                    Value::Null,
                    "frame_too_large",
                    "request frame exceeds 4 MiB",
                )?;
                continue;
            }
            let frame_bytes = bytes.len();
            let value: Value = match serde_json::from_slice(&bytes) {
                Ok(v) => v,
                Err(_) => {
                    error(
                        &reader_out,
                        Value::Null,
                        "parse_error",
                        "invalid JSON frame",
                    )?;
                    continue;
                }
            };
            if value["method"] == "cancel" {
                if value.as_object().is_none_or(|m| {
                    m.keys()
                        .any(|k| !["id", "method", "params"].contains(&k.as_str()))
                }) || value["params"]
                    .as_object()
                    .is_none_or(|m| m.len() != 1 || !m.contains_key("request_id"))
                    || key(&value["params"]["request_id"]).is_none()
                {
                    error(
                        &reader_out,
                        value.get("id").cloned().unwrap_or(Value::Null),
                        "invalid_params",
                        "cancel requires only params.request_id",
                    )?;
                    continue;
                }
                let id = value.get("id").cloned().unwrap_or(Value::Null);
                let token = key(&value["params"]["request_id"])
                    .and_then(|k| reader_active.lock().unwrap().get(&k).cloned());
                let found = token.is_some();
                if let Some(t) = token {
                    t.store(true, Ordering::Relaxed);
                }
                if !id.is_null() {
                    emit(
                        &reader_out,
                        json!({"protocol_version":1,"id":id,"result":{"cancellation_requested":found}}),
                    )?;
                }
                continue;
            }
            let id = value.get("id").cloned().unwrap_or(Value::Null);
            let request: Request = match serde_json::from_value(value) {
                Ok(r) => r,
                Err(e) => {
                    error(&reader_out, id, "invalid_request", &e.to_string())?;
                    continue;
                }
            };
            let Some(k) = key(&request.id) else {
                error(
                    &reader_out,
                    request.id,
                    "invalid_request",
                    "id must be a nonempty string <=128 bytes or unsigned integer",
                )?;
                continue;
            };
            let token = Arc::new(AtomicBool::new(false));
            {
                let mut active = reader_active.lock().unwrap();
                if active.contains_key(&k) {
                    error(
                        &reader_out,
                        request.id,
                        "duplicate_id",
                        "id is already active",
                    )?;
                    continue;
                }
                active.insert(k.clone(), token.clone());
            }
            if reader_bytes
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                    n.checked_add(frame_bytes).filter(|n| *n <= QUEUE_BYTES)
                })
                .is_err()
            {
                reader_active.lock().unwrap().remove(&k);
                error(
                    &reader_out,
                    request.id,
                    "busy",
                    "queued frames exceed 8 MiB; coalesce pending requests",
                )?;
                continue;
            }
            match tx.try_send(Work {
                frame_bytes,
                request,
                cancel: token,
            }) {
                Ok(()) => {}
                Err(mpsc::TrySendError::Full(work)) => {
                    reader_bytes.fetch_sub(frame_bytes, Ordering::Relaxed);
                    reader_active.lock().unwrap().remove(&k);
                    error(
                        &reader_out,
                        work.request.id,
                        "busy",
                        "request queue is full; retry after pending requests finish",
                    )?;
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    reader_bytes.fetch_sub(frame_bytes, Ordering::Relaxed);
                    break;
                }
            }
        }
        Ok(())
    });
    let mut state = None;
    for work in rx {
        let id = work.request.id.clone();
        let result = dispatch(&mut state, &work, &out);
        queued_bytes.fetch_sub(work.frame_bytes, Ordering::Relaxed);
        active
            .lock()
            .unwrap()
            .remove(&key(&work.request.id).unwrap());
        match result {
            Ok(value) => emit(&out, json!({"protocol_version":1,"id":id,"result":value}))?,
            Err(e) => {
                let code = if e.to_string() == "method_not_found" {
                    "method_not_found"
                } else {
                    classify(&e)
                };
                error(&out, id, code, &e.to_string())?;
            }
        }
    }
    reader
        .join()
        .map_err(|_| anyhow::anyhow!("worker input thread failed"))??;
    Ok(())
}
