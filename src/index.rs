use crate::{
    corpus,
    model::{Chunk, Evidence, Response, bounded_response},
    search::SearchOptions,
    structure,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Serialize, Deserialize, Default)]
struct Cache {
    version: u8,
    files: BTreeMap<String, CachedFile>,
}
#[derive(Serialize, Deserialize)]
struct CachedFile {
    hash: String,
    chunks: Vec<Chunk>,
}

pub fn cache_dir(root: &Path) -> Result<PathBuf> {
    let base = directories::ProjectDirs::from("dev", "agentgrep", "agentgrep")
        .context("Cannot locate user cache directory")?;
    let dir = base.cache_dir().join(
        blake3::hash(root.to_string_lossy().as_bytes())
            .to_hex()
            .as_str(),
    );
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}

fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(path.parent().context("Missing cache parent")?)?;
    serde_json::to_writer(&mut file, value)?;
    file.flush()?;
    file.persist(path).map_err(|e| e.error)?;
    Ok(())
}

pub fn refresh(root: &Path, globs: &[String], hidden: bool) -> Result<(Vec<Chunk>, Vec<String>)> {
    let dir = cache_dir(root)?;
    let filter = serde_json::to_vec(&(globs, hidden))?;
    let path = dir.join(format!("index-{}.json", blake3::hash(&filter).to_hex()));
    let mut warnings = vec![];
    let mut cache = match std::fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice::<Cache>(&bytes) {
            Ok(c) if c.version == 1 => c,
            _ => {
                warnings.push("Index cache invalid; rebuilding from current source".into());
                Cache::default()
            }
        },
        Err(_) => Cache::default(),
    };
    let (sources, load_warnings) = corpus::load(root, globs, hidden)?;
    warnings.extend(load_warnings);
    let mut next = BTreeMap::new();
    let mut changed = cache.version != 1;
    for source in sources {
        let hash = blake3::hash(source.content.as_bytes()).to_hex().to_string();
        let existing = cache.files.remove(&source.path).filter(|f| f.hash == hash);
        let value = existing.unwrap_or_else(|| {
            changed = true;
            CachedFile {
                hash,
                chunks: structure::chunks(&source),
            }
        });
        next.insert(source.path, value);
    }
    changed |= !cache.files.is_empty();
    let next = Cache {
        version: 1,
        files: next,
    };
    if changed {
        atomic_json(&path, &next)?;
    }
    Ok((
        next.files.into_values().flat_map(|f| f.chunks).collect(),
        warnings,
    ))
}

pub fn terms(text: &str) -> Vec<String> {
    let mut separated = String::new();
    let mut previous_lower = false;
    for c in text.chars() {
        if c.is_uppercase() && previous_lower {
            separated.push(' ');
        }
        if c.is_alphanumeric() {
            separated.extend(c.to_lowercase());
        } else {
            separated.push(' ');
        }
        previous_lower = c.is_lowercase() || c.is_ascii_digit();
    }
    separated
        .split_whitespace()
        .filter(|s| {
            !matches!(
                *s,
                "a" | "an"
                    | "the"
                    | "is"
                    | "are"
                    | "how"
                    | "where"
                    | "what"
                    | "which"
                    | "does"
                    | "do"
                    | "of"
                    | "to"
                    | "and"
                    | "in"
                    | "for"
                    | "this"
                    | "that"
                    | "with"
                    | "it"
                    | "we"
            )
        })
        .map(str::to_owned)
        .collect()
}

fn bm25(chunks: &[Chunk], query: &str) -> Vec<(usize, f64)> {
    if chunks.is_empty() {
        return vec![];
    }
    let query: HashSet<_> = terms(query).into_iter().collect();
    let docs: Vec<_> = chunks.iter().map(|c| terms(&c.content)).collect();
    let average = (docs.iter().map(Vec::len).sum::<usize>() as f64 / docs.len() as f64).max(1.0);
    let mut dfs = HashMap::new();
    for doc in &docs {
        for term in doc.iter().collect::<HashSet<_>>() {
            *dfs.entry(term).or_insert(0usize) += 1;
        }
    }
    let mut scores = Vec::new();
    for (id, doc) in docs.iter().enumerate() {
        let mut score = 0.0;
        for term in &query {
            let tf = doc.iter().filter(|t| *t == term).count() as f64;
            let df = *dfs.get(term).unwrap_or(&0) as f64;
            let idf = (1.0 + (docs.len() as f64 - df + 0.5) / (df + 0.5)).ln();
            score += idf * tf * 2.2 / (tf + 1.2 * (0.25 + 0.75 * doc.len() as f64 / average));
            if terms(chunks[id].symbol.as_deref().unwrap_or("")).contains(term) {
                score += idf * 1.5;
            }
            if terms(&chunks[id].path).contains(term) {
                score += idf * 0.3;
            }
        }
        if score > 0.0 {
            scores.push((id, score));
        }
    }
    scores.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    scores
}

fn embed(model: &str, texts: &[String]) -> Result<Vec<Vec<f64>>> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(60)))
        .build();
    let agent: ureq::Agent = config.into();
    let value: serde_json::Value = agent
        .post("http://127.0.0.1:11434/api/embed")
        .send_json(serde_json::json!({"model":model,"input":texts,"truncate":true}))
        .context(
            "Local Ollama embedding request failed; start Ollama and pull the requested model",
        )?
        .body_mut()
        .read_json()?;
    let vectors: Vec<Vec<f64>> = serde_json::from_value(
        value
            .get("embeddings")
            .context("Ollama returned no embeddings")?
            .clone(),
    )?;
    anyhow::ensure!(
        vectors.len() == texts.len()
            && vectors
                .iter()
                .all(|v| !v.is_empty() && v.iter().all(|x| x.is_finite())),
        "Invalid Ollama embedding response"
    );
    Ok(vectors)
}

fn cosine(a: &[f64], b: &[f64]) -> Result<f64> {
    anyhow::ensure!(
        a.len() == b.len(),
        "Embedding dimensions changed; clear index cache and retry"
    );
    let dot = a.iter().zip(b).map(|(x, y)| x * y).sum::<f64>();
    let norm =
        a.iter().map(|x| x * x).sum::<f64>().sqrt() * b.iter().map(|x| x * x).sum::<f64>().sqrt();
    Ok(if norm == 0.0 { 0.0 } else { dot / norm })
}

fn semantic(root: &Path, chunks: &[Chunk], query: &str, model: &str) -> Result<Vec<(usize, f64)>> {
    let path = cache_dir(root)?.join(format!(
        "embeddings-{}.json",
        blake3::hash(model.as_bytes()).to_hex()
    ));
    let mut vectors: BTreeMap<String, Vec<f64>> = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let inputs: Vec<_> = chunks
        .iter()
        .map(|c| {
            format!(
                "{}\n{}\n{}",
                c.path,
                c.symbol.as_deref().unwrap_or(""),
                c.content
            )
        })
        .collect();
    let keys: Vec<_> = inputs
        .iter()
        .map(|s| blake3::hash(s.as_bytes()).to_hex().to_string())
        .collect();
    let missing: Vec<_> = inputs
        .iter()
        .zip(&keys)
        .filter(|(_, key)| !vectors.contains_key(*key))
        .collect();
    let changed = !missing.is_empty() || vectors.len() > keys.len();
    for batch in missing.chunks(16) {
        let texts = batch.iter().map(|(s, _)| (*s).clone()).collect::<Vec<_>>();
        for ((_, key), vector) in batch.iter().zip(embed(model, &texts)?) {
            vectors.insert((*key).clone(), vector);
        }
    }
    let live: HashSet<_> = keys.iter().collect();
    vectors.retain(|key, _| live.contains(key));
    if changed {
        atomic_json(&path, &vectors)?;
    }
    let query_vector = embed(model, &[query.to_owned()])?.remove(0);
    let mut scores: Vec<_> = keys
        .iter()
        .enumerate()
        .map(|(id, key)| Ok((id, cosine(&query_vector, &vectors[key])?)))
        .collect::<Result<_>>()?;
    scores.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    Ok(scores)
}

pub fn ranked(root: &Path, options: &SearchOptions) -> Result<Response> {
    let (chunks, warnings) = refresh(root, &options.glob, options.hidden)?;
    let mut scores = bm25(&chunks, &options.query);
    if options.mode == "hybrid" {
        let model = options
            .model
            .as_deref()
            .context("hybrid mode requires --model (a locally installed Ollama embedding model)")?;
        let semantic = semantic(root, &chunks, &options.query, model)?;
        let mut fused = HashMap::new();
        for ranking in [&scores, &semantic] {
            for (rank, (id, _)) in ranking.iter().take(100).enumerate() {
                *fused.entry(*id).or_insert(0.0) += 1.0 / (60.0 + rank as f64 + 1.0);
            }
        }
        scores = fused.into_iter().collect();
        scores.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    }
    let mut hits: Vec<Evidence> = vec![];
    for (id, score) in scores {
        let chunk = &chunks[id];
        // Suppress redundant overlapping windows/declarations in ranked results.
        if hits.iter().any(|h| {
            if h.chunk.path != chunk.path {
                return false;
            }
            let overlap = chunk
                .end_line
                .min(h.chunk.end_line)
                .saturating_sub(chunk.start_line.max(h.chunk.start_line))
                + usize::from(
                    chunk.end_line.min(h.chunk.end_line)
                        >= chunk.start_line.max(h.chunk.start_line),
                );
            overlap * 2
                >= (chunk.end_line - chunk.start_line + 1)
                    .min(h.chunk.end_line - h.chunk.start_line + 1)
        }) {
            continue;
        }
        hits.push(Evidence {
            chunk: chunk.clone(),
            score,
            match_lines: vec![],
            excerpt_truncated: false,
        });
    }
    Ok(bounded_response(
        root,
        &options.mode,
        &options.query,
        hits,
        options.limit,
        options.budget_bytes,
        warnings,
    ))
}
