# CLI JSON contract for initial editor integration

The existing command remains:

```sh
cargo install --path . --locked
agx search QUERY ROOT --mode text
agx search QUERY ROOT --mode symbol
agx search QUERY ROOT --mode ranked
```

Use the normal `agx` executable and structured subprocess arguments. Example argv: `["search", "session token", "/Users/me/My Project", "--mode", "ranked"]`. Do not concatenate a shell command. Queries beginning with `-` can use an argument terminator: put the options first, then `--`, query, root. JSON is the default; do not pass `--pretty` in an adapter.

These three modes perform only local text/syntax/lexical operations and never invoke embeddings, Ollama, inference services, model downloads, telemetry, or automatic updates. `--model` with these modes is an error. Standalone hybrid remains explicitly selected with `--mode hybrid --model NAME`; the nain adapter must never select it. BM25 is lexical term ranking, not semantic model inference.

## Response schema 1

One UTF-8 JSON object plus a newline on stdout:

```json
{"schema_version":1,"root":"/Users/me/My Project","mode":"text","query":"needle","results":[{"path":"app.py","start_line":1,"end_line":2,"symbol":null,"kind":"lines","content":"needle\nnext line","score":1.0,"match_lines":[1],"excerpt_truncated":false}],"matched_units":1,"returned_units":1,"truncated":false,"budget_bytes":16000,"warnings":[],"incomplete":false}
```

| Field | Type and meaning |
| --- | --- |
| `schema_version` | Integer 1. Accept optional/additive fields rather than rejecting new keys. |
| `root` | Canonical absolute root string. The adapter supplies its own workspace/root identity for each subprocess. |
| `mode`, `query` | Strings identifying the requested operation. |
| `results` | Ordered array of evidence units described below. |
| `matched_units` | Integer count after declaration/range deduplication and ranked overlap suppression, before output limits. Not raw matching-line count. |
| `returned_units` | Integer array length. |
| `truncated` | Boolean: result units omitted by limit/budget or source excerpts clipped. |
| `budget_bytes` | Total UTF-8 source excerpt budget, default 16000. Metadata/JSON escaping is outside this CLI budget. |
| `warnings` | Array of warning strings; inspect even on exit 0. |
| `incomplete` | Additive boolean indicating eligible source was omitted due to a read, size, binary-content, UTF-8 or traversal warning. Older schema-1 binaries may omit it. Rebuilt invalid-cache warnings alone do not mark source incomplete. |

Evidence fields: root-relative `path`; 1-based inclusive `start_line`/`end_line`; nullable `symbol`; string `kind` (grammar declaration, `lines`, `window`); verbatim LF-normalized source excerpt `content`; floating-point `score`; array of 1-based `match_lines`; boolean `excerpt_truncated`. A clipped excerpt does not shorten the full source range. Text/symbol scores are 1.0; ranked scores are relative BM25 scores and ranked `match_lines` is empty. No model creates excerpts or symbol names. Symbol is syntactic containment, not a language-server/type-resolution guarantee.

Open results at zero-based `(start_line-1,0)`. Schema 1 has no column offsets, document versions, session IDs or stable filesystem snapshot. Associate responses with the initiating query/project generation and discard superseded subprocess results. This CLI searches disk, not unsaved buffers; the worker is the supported path for document overrides. The CLI reads eligible source on each call; ranked reuses content-hash syntax caches but recomputes term statistics. Reopening a location must use the editor's current buffer, not assume the excerpt is still current.

Default scope respects ignore rules, hides hidden files, skips symlinks, build/dependency directories and unsupported binary extensions. Maximum eligible source file is 2 MiB. Oversized, NUL-containing, invalid UTF-8 and unreadable eligible files now report warnings; expected ignore/format exclusions are scope filters rather than enumerated failures. `incomplete:false` does not mean that ignored files, documents, binaries, or unknown file types were searched. Extracted-document line numbers refer to extracted Markdown, not original pages; nain's integration must not invoke the parser.

Limits: `--limit` 1–1000 (default 20), `--budget-bytes` 1–1000000, `--context` 0–100 (default 2). Positive `--glob/-g` includes and `!` exclusions filter relative paths. A glob with no `/` matches basenames throughout the tree. Smart-case regex is default for text/symbol; `--literal/-F` escapes regex, `--case-sensitive` forces case sensitivity. Ranked splits lexical identifiers and removes stop words; regex/case/context flags do not affect BM25. CLI language filtering can be implemented as extension globs; the worker additionally has language-name filters.

## Exit codes and errors

- **0:** Successful JSON response, including zero matches, truncated results or warning/incomplete results. No ripgrep-style exit 1 for no matches. Broken output pipes are handled as clean exits.
- **2:** Runtime or CLI-usage failure. Runtime errors write one `{"error":"descriptive chained message"}` JSON object to stderr; stdout is empty. Clap argument/usage errors instead write human-readable diagnostics to stderr. Do not assume all stderr is JSON.
- Signal termination/crashes use the operating system's process status and are adapter failures, not search results.

Invalid regex, empty query, invalid limits, missing/inaccessible root and forbidden model selection are errors. Partial per-file read failures can produce successful responses with warnings; never silently label these a complete “no matches” result. Validate the schema and frame length before using output. CLI serialized output has no hard total-byte cap; adapters should bound captured output and terminate on excess. Worker protocol v1 adds a hard serialized search-response cap, cancellation and freshness identifiers.
