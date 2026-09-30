# Roadmap

## Included in 0.1

- Native Rust CLI with JSON evidence and explicit source/response limits.
- Ignore-aware text search, smart case, literal/regex modes, globs.
- Syntactic enclosing declarations for Rust, Python, TS/JS, Go, Swift.
- Content-hash refreshed BM25 indexing and overlapping source windows.
- Optional local Ollama embeddings, cosine ranking, reciprocal rank fusion.
- Repository map, source-range reader, cache cleanup, capability report.
- External LiteParse Markdown extraction.
- Stdio MCP, Codex/Claude skills, Claude local skill plugin.
- Portable tests and Linux/Apple Silicon/Intel Mac CI.

## Next: measure before optimizing

1. Run Mac CI, execute on real Apple Silicon/Intel machines, and test actual Codex/Claude harness sessions.
2. Collect navigation tasks across Swift, TS, Rust, Python and document corpora. Evaluate recall@k, inspected-source bytes, time to verified evidence, and coding-task success against ripgrep and supplied tools. Include cold/warm states and failure cases.
3. Add tokenizer adapters and a hard serialized response budget; retain explicit omission reasons and continuation paths.
4. Preserve LiteParse JSON page/bounding-box provenance, extraction hashes, and citations through search results.

## Larger repositories

- SQLite/FTS or compact postings index with transactional snapshots and query-time freshness checks.
- Optional watcher/daemon and model lifetime management after measuring startup costs.
- Approximate vector retrieval only when corpus size warrants it.
- Finer syntax units, tree-sitter queries per language, and symbol navigation.
- Type-aware call edges via language servers, with uncertainty annotations for dynamic dispatch; no unsupported impact certainty.

## Distribution

- v0.2 implements Developer ID signing/notarization, stapled DMGs, verified self-updates, rollback, and a daily opt-in LaunchAgent. Actual signed publication is pending Apple credentials; see [setup](signing-and-updates.md).
- A real Homebrew tap remains future distribution work.
- Versioned MCP/output contract fixtures and harness compatibility tests.
- Keep default installation free of model downloads, remote provider accounts, and enforced harness tool replacement.
