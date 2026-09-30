# Validation · 2026-09-30

Validated locally on Linux x86_64 using Rust 1.98.1 and a release build. The macOS CI matrix is configured but was not executed here. A real Claude Code or Codex agent task using the tool was not evaluated.

## Passed

- `cargo fmt --check`
- `cargo clippy --all-targets --locked -- -D warnings`
- `cargo test --locked`: 14 integration tests covering matching, ignores/globs, binary/document filtering, supported syntax grammars including Swift, declaration deduplication, UTF-8 budgets, omissions, index freshness/filter isolation, cache recovery/permissions, read confinement, symlinks, size limits, skills, and MCP behavior.
- `cargo build --release --locked`
- Official `@modelcontextprotocol/sdk` 1.31.0: stdio connection, tool listing, structural search, source reading, and clean close.
- `scripts/check_optional.py`: fixture Ollama requests, hybrid ranking plumbing, vector cache reuse and changed-chunk refresh, missing-model/invalid-vector errors. Deterministic fixture vectors validate the adapter, not semantic quality.
- `scripts/check_optional.py --liteparse` with `@llamaindex/liteparse` 2.15.0: actual one-page PDF with spaces in its filename, Markdown extraction, search, provenance output, and existing-output preservation.

## Synthetic benchmark

Command: `python3 scripts/benchmark.py --binary target/release/agx --files 1000`. Corpus: 1,000 Python files, 60 lines each. Times include process startup. Repeated operations report the median of five runs; cold ranking is one run. This is one local execution, not statistically robust results or Mac performance.

| Operation | Time (ms) | Stdout bytes |
| --- | ---: | ---: |
| Exact, 8-unit limit | 12.51 | 2,461 |
| Symbol, 8-unit limit | 18.63 | 2,378 |
| Ranked cold, 8-unit limit | 628.69 | 2,464 |
| Ranked warm, 8-unit limit | 124.78 | 2,464 |
| ripgrep, all matching lines | 5.93 | 169,340 |

Outputs have different scope: bounded evidence versus all raw matching lines. This demonstrates response size control, not an equivalent-results speed comparison or agent token/cost savings. Ripgrep was faster for exact matching. Ranked queries still read/hash current files and compute BM25 over cached chunks; warm retrieval remains linear in corpus size.

## Pending

Run Mac CI and actual harness sessions before release. Evaluate real embedding models, heterogeneous repositories, complex documents, and navigation-task recall. Source budgets exclude metadata/JSON overhead. Large-corpus scaling, page-aware citations, type-resolved graphs, signed binaries, and a Homebrew tap remain future work.
