use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub symbol: Option<String>,
    pub kind: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    #[serde(flatten)]
    pub chunk: Chunk,
    pub score: f64,
    pub match_lines: Vec<usize>,
    pub excerpt_truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct Response {
    pub schema_version: u8,
    pub root: String,
    pub mode: String,
    pub query: String,
    pub results: Vec<Evidence>,
    pub matched_units: usize,
    pub returned_units: usize,
    pub truncated: bool,
    pub budget_bytes: usize,
    pub warnings: Vec<String>,
}

// The budget bounds source UTF-8 bytes, not model-specific tokens or JSON overhead.
pub fn bounded_response(
    root: &std::path::Path,
    mode: &str,
    query: &str,
    mut hits: Vec<Evidence>,
    limit: usize,
    budget: usize,
    warnings: Vec<String>,
) -> Response {
    hits.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.chunk.path.cmp(&b.chunk.path))
            .then(a.chunk.start_line.cmp(&b.chunk.start_line))
    });
    let matched_units = hits.len();
    let mut remaining = budget;
    let mut results = Vec::new();
    for mut hit in hits.into_iter().take(limit) {
        if remaining == 0 {
            break;
        }
        if hit.chunk.content.len() > remaining {
            let mut end = remaining;
            while !hit.chunk.content.is_char_boundary(end) {
                end -= 1;
            }
            hit.chunk.content.truncate(end);
            hit.excerpt_truncated = true;
        }
        remaining -= hit.chunk.content.len();
        let clipped = hit.excerpt_truncated;
        results.push(hit);
        if clipped {
            break;
        }
    }
    let returned_units = results.len();
    let truncated = returned_units < matched_units || results.iter().any(|h| h.excerpt_truncated);
    Response {
        schema_version: 1,
        root: root.display().to_string(),
        mode: mode.into(),
        query: query.into(),
        results,
        matched_units,
        returned_units,
        truncated,
        budget_bytes: budget,
        warnings,
    }
}
