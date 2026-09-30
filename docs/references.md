# Reference analysis

Reviewed the supplied URLs on 2026-09-30. Project README claims below describe those projects; they are not independently reproduced benchmark results.

| Reference | Useful idea | Decision in agentgrep |
| --- | --- | --- |
| [jevgrep](https://github.com/dzhng/jevgrep) | Explore unknown behavior and return source leads/excerpts; install a skill alongside the CLI | Evidence first and bundled skills; local lexical discovery initially, optional local embeddings; no hosted relevance agent |
| [LlamaIndex: lexical vs semantic search](https://www.llamaindex.ai/blog/is-grep-all-you-need-lexical-vs-sematic-search-for-agents) | Exact search is strong for known tokens; vocabulary and corpus scale need different retrieval; parse documents before search | Distinct exact, ranked lexical, hybrid, and document-extraction paths; explicit retrieval limits |
| [hypergrep](https://github.com/marjoballabani/hypergrep) | Enclosing syntax, compact context, repeated-query indexing, agent setup | Tree-sitter declarations, byte budgets, refreshed local cache, repository map; defer type-resolved graphs and daemon |
| [ai-grep](https://github.com/moinulmoin/ai-grep) | Local embeddings, AST chunks, incremental state, reranking | Optional Ollama embeddings plus syntax/window chunks; no bundled model runtime or neural reranker in v0.1 |
| [CodeAnt: agents and ripgrep](https://codeant.ai/blogs/why-coding-agents-should-use-ripgrep) | Recursive ignore-aware traversal, smart case, filters, less irrelevant context | Rust `ignore`/`regex` foundations, parallel file loading, smart case and glob filters |
| [LiteParse](https://github.com/run-llama/liteparse) | Fast local layout-aware parsing, OCR, Markdown/JSON and agent skills | External `lit` adapter for explicit Markdown extraction; do not duplicate a document parser |

We do not inherit performance, solve-rate, token-saving, cost-saving, recall, or false-negative claims from these tools. AST-based call guesses do not establish a sound dependency graph, and a bloom-filter absence claim only applies when indexing/tokenization assumptions hold. Our initial release avoids both claims.

The LlamaIndex article argues for layered retrieval rather than one universally sufficient search primitive. We follow that distinction: ranked BM25 remains lexical; only the explicit hybrid mode includes embeddings. Natural-language input alone does not make a tool semantic.

The linked repositories' top-level READMEs were reviewed rather than vendoring their implementations. Original code in this repository is MIT licensed. LiteParse is an optional external Apache-2.0 project; see upstream and Cargo dependency licenses when distributing binaries.
