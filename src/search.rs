use crate::{
    corpus,
    model::{Chunk, Evidence, Response, bounded_response},
    structure,
};
use anyhow::Result;
use regex::RegexBuilder;
use std::{collections::BTreeMap, path::Path};

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SearchOptions {
    pub query: String,
    pub mode: String,
    pub glob: Vec<String>,
    pub hidden: bool,
    pub literal: bool,
    pub case_sensitive: bool,
    pub context: usize,
    pub limit: usize,
    pub budget_bytes: usize,
    pub model: Option<String>,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            query: String::new(),
            mode: "text".into(),
            glob: vec![],
            hidden: false,
            literal: false,
            case_sensitive: false,
            context: 2,
            limit: 20,
            budget_bytes: 16000,
            model: None,
        }
    }
}

pub fn validate(options: &SearchOptions) -> Result<()> {
    anyhow::ensure!(!options.query.trim().is_empty(), "Query cannot be empty");
    anyhow::ensure!(
        options.limit > 0 && options.limit <= 1000,
        "limit must be between 1 and 1000"
    );
    anyhow::ensure!(
        options.budget_bytes > 0 && options.budget_bytes <= 1_000_000,
        "budget_bytes must be between 1 and 1000000"
    );
    anyhow::ensure!(options.context <= 100, "context must be at most 100");
    anyhow::ensure!(
        options.mode == "hybrid" || options.model.is_none(),
        "model is only allowed in standalone hybrid mode"
    );
    Ok(())
}

pub fn search(root: &Path, options: &SearchOptions) -> Result<Response> {
    validate(options)?;
    if matches!(options.mode.as_str(), "ranked" | "hybrid") {
        return crate::index::ranked(root, options);
    }
    anyhow::ensure!(
        matches!(options.mode.as_str(), "text" | "symbol"),
        "Unknown mode {}",
        options.mode
    );
    let pattern = if options.literal {
        regex::escape(&options.query)
    } else {
        options.query.clone()
    };
    let regex = RegexBuilder::new(&pattern)
        .case_insensitive(!options.case_sensitive && !options.query.chars().any(char::is_uppercase))
        .build()?;
    let (sources, warnings) = corpus::load(root, &options.glob, options.hidden)?;
    let mut hits = Vec::new();
    for source in &sources {
        let lines: Vec<_> = source.content.lines().collect();
        let matches: Vec<_> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| regex.is_match(l))
            .map(|(n, _)| n + 1)
            .collect();
        if matches.is_empty() {
            continue;
        }
        let declarations = if options.mode == "symbol" {
            structure::symbols(source)
        } else {
            vec![]
        };
        let mut units: BTreeMap<(usize, usize), Evidence> = BTreeMap::new();
        for line in matches {
            let symbol = declarations
                .iter()
                .filter(|c| c.start_line <= line && c.end_line >= line)
                .min_by_key(|c| c.end_line - c.start_line);
            let chunk = if let Some(symbol) = symbol {
                symbol.clone()
            } else {
                let start = line.saturating_sub(options.context + 1);
                let end = (line + options.context).min(lines.len());
                Chunk {
                    path: source.path.clone(),
                    start_line: start + 1,
                    end_line: end,
                    symbol: None,
                    kind: "lines".into(),
                    content: lines[start..end].join("\n"),
                }
            };
            let hit = units
                .entry((chunk.start_line, chunk.end_line))
                .or_insert(Evidence {
                    chunk,
                    score: 1.0,
                    match_lines: vec![],
                    excerpt_truncated: false,
                });
            hit.match_lines.push(line);
        }
        hits.extend(units.into_values());
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
