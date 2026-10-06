# Validation · 2026-09-30

The original v0.1 local checks below were recorded on Linux x86_64 using Rust 1.98.1. Later native CI/release checks are published in GitHub Actions; see the v0.3 editor validation section. A real Claude Code or Codex agent task using the tool was not evaluated.

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

Evaluate actual harness sessions, real embedding models, heterogeneous repositories, complex documents, and navigation-task recall. Source budgets exclude metadata/JSON overhead. Large-corpus scaling, page-aware citations, type-resolved graphs, and a Homebrew tap remain future work.

## v0.2.0 signing and updater validation

Local Linux checks passed: formatting, Clippy with warnings denied, 7 Rust unit tests, 15 integration tests, 11 Python signing/publication tests, and an optimized release build. Optional Ollama fixture and real LiteParse PDF checks passed again. An actual GitHub update check passed using platform certificate verification.

Signing tests mock Apple services; they verify hardened-runtime signing arguments, notarization rejection, ticket stapling, temporary-keychain cleanup, and manual-only publication policy. They do not prove Apple notarization. Mac CI additionally checks that real codesign rejects an unsigned executable. Actual Developer ID signing, Apple notarization, Gatekeeper acceptance, and a complete signed update require Apple credentials, which are not configured yet.

Tag builds and ordinary CI remain unsigned. Signing requires a manual workflow run with `sign_and_notarize=true`; the publication script independently rejects signed requests from automatic events. Unsigned releases are development prereleases and cannot be installed by the automatic updater.

## v0.3 editor worker validation

Local Linux passed formatting, Clippy with warnings denied, 7 updater unit tests, 15 CLI/MCP integration tests, the Rust adapter executable-path test, and 26 Python tests (15 real editor-worker subprocess tests plus 11 signing/publication tests). Normal `cargo install --path . --locked` was validated using a separate installation prefix. The native Rust subprocess adapter negotiated, indexed, searched, and checked session/schema successfully.

Worker regressions cover capability/unsupported-version negotiation, CLI schema compatibility, model/hybrid/unknown-operation rejection, a live local Ollama listener receiving zero calls and an executable trap remaining unused, overlays and monotonic versions, save/close/deletion behavior, index fences, incremental BM25 versus a fresh index, multi-root confinement and symlinks, nested ignore precedence and confined ignore rules, CRLF/Unicode ranges, match-line limits, file/memory/response limits, malformed/oversized frames, duplicate active IDs, and indexing/search cancellation with recovery. Warm queries retain cached content until an explicit disk event/rescan; this behavior is tested rather than mistaken for filesystem freshness.

The [cold/warm benchmark](editor-benchmarks.md) includes raw local results and limitations. Native Apple Silicon and Intel [CI run 37493947952](https://github.com/ar4ft/agentgrep/actions/runs/37493947952) passed all checks: 24 Rust tests (including the real unsigned-signature rejection), 26 Python tests, Cargo installation, the native adapter example and the benchmark. Raw reports are linked in the benchmark guide and published as artifacts. The release matrix also repeats native tests/builds and publishes per-platform benchmark JSON with unsigned development archives. Actual nain GUI behavior, hostile concurrent filesystem mutation isolation, signed Apple publication, peak-memory studies and real-agent task quality remain unvalidated here. No nain source files were modified.

## v0.3.1 installer validation

Local Linux passed formatting, Clippy with warnings denied, 23 Rust tests, 35 Python tests, an optimized build, native packaging, optional Ollama fixture checks, shell syntax checks, and workflow linting. Eight installer tests run the script through stdin with the actual native executable and real tar/checksum tools; only HTTP transport is replaced with fixtures. They cover native execution, repeat installation/backup, profile preservation/idempotence, quoted custom paths, compact/pretty metadata, production-channel rejection of development builds, corrupt downloads, unsafe archives/links, version mismatch, unmanaged files, symlinks, and installer lock conflicts. Publication tests require the uploaded installer and verify its aggregate checksum entry.

A real public download/installation of v0.3.0 also succeeded on Linux before publication. Native CI repeats the regression suite on Apple Silicon and Intel. The release workflow additionally downloads the published `install.sh` and installs its matching public archive on both Mac architectures and Linux, then checks the version and all three lexical search modes. See [GitHub Actions](https://github.com/ar4ft/agentgrep/actions) for the completed run status; a configured workflow alone is not proof of a successful Mac install. The bootstrap installer uses GitHub HTTPS and checksums, not independent Apple publisher verification. Signing/Gatekeeper and production automatic update validation still require Apple credentials.

## v0.3.3 release discovery fallback

[v0.3.1 native CI](https://github.com/ar4ft/agentgrep/actions/runs/37505388817) passed on both Macs and Linux. Its release published successfully and public installation passed on Intel Mac/Linux, but the Apple Silicon public-download job was blocked by GitHub API HTTP 403 twice, before downloading the binary. This exposed a discovery dependency rather than a binary/test failure.

v0.3.3 adds public Atom feed discovery when API access is unavailable, and direct asset selection for explicit pins. The feed includes bare tags; fallback discovery probes native checksum availability and skips tags whose assets are not uploaded yet. Both paths warn that release classification is unknown; production-only installs still require API classification. Ten installer regressions now cover this behavior, escaped feed contents and invalid tags, in addition to the earlier installer checks. Local Linux passed formatting, Clippy, 23 Rust tests, 37 Python tests, release build and workflow linting. A genuine GitHub feed/archive/checksum install succeeded with only API requests deliberately blocked. Native CI repeats these tests, and post-publication jobs exercise both pinned and default installation on all three platforms. API/feed availability, release feed format/cache freshness, and independent Apple bootstrap verification remain external dependencies/limitations.
