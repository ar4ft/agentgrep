# agentgrep · `agx`

Local source evidence for Codex, Claude Code, and other LLM agents. A native Rust binary with exact search, syntax context, ranked discovery, optional local embeddings, and MCP. macOS on Apple Silicon and Intel is the primary target; Linux is supported for development and CI.

**Version 0.3: restricted local editor worker and nain integration contract.** Releases remain unsigned development prereleases until Apple signing credentials are configured. This is a working foundation, not a benchmarked replacement for ripgrep. No hosted inference calls, API keys, external `rg` executable, or model downloads are needed for text, symbol, or ranked search.

## Local editor integration (nain)

Install the existing executable with `cargo install --path . --locked`. The JSON CLI remains available as `agx search QUERY ROOT --mode text|symbol|ranked`. These modes do not invoke models; `--model` is rejected outside standalone hybrid search.

`agx serve --stdio --restricted` adds a persistent, local editor worker with cached syntax/BM25 data, incremental file updates, unsaved document versions, cancellation, indexing progress, bounded search responses, and explicit incomplete results. The worker exposes only Text, Symbol, and Ranked; BM25 is lexical ranking, not model inference. Standalone agent, hybrid, MCP, parsing, and update capabilities remain available separately.

See the [CLI JSON contract](docs/cli-json.md), [worker protocol v1](docs/editor-protocol.md), and [nain integration guide](docs/nain-integration.md). The guide describes executable discovery, the optional Rust subprocess example, and the native Code Search panel/provider interface nain must add. **Current nain has no custom sidebar/search-provider extension API: installing agx or an adapter does not create a panel.** No nain code is changed here.

## Signed releases and updates

Normal CI/tag builds create unsigned development binaries. Only a manually started release workflow with `sign_and_notarize=true` signs Mac binaries, requires accepted Apple notarization, and produces DMGs with stapled tickets. Mac automatic updates verify the publisher identity and retain a rollback copy. Activation requires the one-time [Apple/GitHub credential setup](docs/signing-and-updates.md); it is not configured yet.

After installing the first signed release:

```sh
agx update check
agx update install
agx update auto enable   # opt in to verified daily background updates
agx update auto status
agx update auto disable
agx update rollback
```

See the [signing and updater guide](docs/signing-and-updates.md) for trust pins, source builds, channels, Command Line Tools, writable installation paths, and rollback behavior. Search and MCP do not check for updates.

For unsigned development releases, use the explicit development updater:

```sh
agx update-pre --check   # discover a development archive, without installing
agx update-pre           # install only if newer; verify SHA-256 and retain rollback
agx update rollback      # restore the exact binary from the last successful update
```

This works on Apple Silicon/Intel Mac and Linux x86_64. It updates the running
`agx` executable's installation path, including `~/.agx/bin/agx`, without sudo or
background updates. It trusts GitHub HTTPS/checksums and does not verify Apple
signatures/notarization. `agx update install --prerelease` still requires a signed
DMG; automatic updates remain signed-only. Versions before 0.3.4 need the installer
below once to gain `update-pre`.

## Install on Mac

Install the prebuilt `agx` into `~/.agx/bin` without Rust or sudo:

```sh
curl -fsSL https://raw.githubusercontent.com/ar4ft/agentgrep/main/scripts/install.sh | sh
```

This selects the newest published release, including unsigned development prereleases,
verifies the archive checksum, and configures your shell PATH. Open a new terminal
or run `. "$HOME/.agx/env"`. Rerun the command to upgrade. Use `--stable` to require
a production release (none is available until Apple setup is complete).
See [installer options and trust guarantees](docs/installation.md) for pinned versions,
custom directories, GUI editor paths, and `--no-modify-path`. The script is also
uploaded automatically as a release asset; it does not enable automatic updates
or configure models/agent harnesses.

For a source build, install Rust and Apple's command-line tools if needed:

```sh
xcode-select --install
brew install rust
```

From this checkout:

```sh
cargo install --path . --locked
agx doctor
```

Cargo installs `agx` into `~/.cargo/bin`; add that directory to your PATH if needed. `cargo build --release --locked` produces `target/release/agx`. The binary embeds the search engine and syntax grammars. Optional LiteParse and Ollama remain separate dependencies.

## Search

```sh
# Exact source evidence; lowercase queries use smart case
agx search 'verifyToken' . --literal

# Return the smallest enclosing function/class rather than isolated lines
agx search 'verifyToken' . --literal --mode symbol -g '*.swift'

# Discover unfamiliar behavior using BM25 and identifier splitting
agx search 'session token authentication' . --mode ranked --limit 8

# Keep source context small; all modes default to machine-readable JSON
agx search 'retry|timeout' src --budget-bytes 12000 --limit 8
agx search 'retry' . --pretty

# Orient, then read only the useful source range
agx map .
agx read src/auth.swift --root . --start 20 --end 80
```

| Mode | Best use | Retrieval |
| --- | --- | --- |
| `text` | Known identifiers, errors, regexes | Current text, line-based Rust regex |
| `symbol` | Implementation context around exact matches | Text matching + smallest enclosing tree-sitter declaration |
| `ranked` | Questions with likely repository vocabulary | BM25, camelCase/snake_case splitting, symbol/path boosts |
| `hybrid` | Vocabulary differs from implementation names | BM25 + local Ollama cosine rankings, reciprocal rank fusion |

Structural support: Rust, Python, TypeScript/TSX, JavaScript/JSX, Go, and Swift. Unsupported languages and matches outside recognized declarations return line context. This is syntactic context, not semantic type resolution. Anonymous functions, arrow functions, and some declaration forms currently fall back to containing declarations or windows.

`-g '*.swift'` includes a glob without overriding hidden/ignore rules; `-g '!**/*test*'` excludes one. `--hidden` includes hidden files while retaining ignore rules. The engine honors `.gitignore` even outside Git repositories, `.ignore`, and global ignore configuration supported by the `ignore` crate. It does **not** load `.rgignore` or `RIPGREP_CONFIG_PATH`. It always skips `.git`, `node_modules`, `target`, `.venv`, `vendor`, `dist`, `build`, and `.agentgrep` directories, symlinks, known binary/document formats (extract PDFs/Office first), binary/non-UTF8 files, and files over 2 MiB. Search roots must be directories. Regexes are line-based; PCRE lookarounds/backreferences and multiline matching are not supported.

### Optional local semantic search

Install/start [Ollama](https://ollama.com), then explicitly download an embedding model:

```sh
ollama pull nomic-embed-text
agx search 'reject requests arriving too quickly' . --mode hybrid --model nomic-embed-text
```

Only hybrid mode sends eligible source chunks to the local service at `127.0.0.1:11434`. `agx` does not configure cloud endpoints. Choose a local embedding model; Ollama's own configuration and model behavior remain under your control. Cold indexing can take time. Each embedding request has a 60-second timeout; indexing issues batches of 16. Failures are explicit rather than silently changing modes. Replacing a model under the same name requires `agx clean .` before reuse. Hybrid considers the top 100 candidates per ranking; relevance and recall are not guaranteed.

### Search documents with LiteParse

```sh
npm install -g @llamaindex/liteparse
# Alternative: install the Python liteparse package in your chosen environment

agx parse specification.pdf --out docs/specification.extracted.md
agx search 'retry policy' docs --mode ranked
```

`parse` delegates to `lit parse --format markdown --image-mode off`, then saves nonempty extracted text to the requested new file. Existing output files are preserved. Source paths with spaces are supported. LiteParse handles conversion/OCR; Office formats may require LibreOffice according to its documentation. Search citations refer to extracted Markdown lines, **not original pages or bounding boxes**. The command prints the original source and extraction paths so callers can retain that relationship. Page-preserving structured ingestion is planned.

## Teach the agent

Run in the project where your agent works:

```sh
agx skill --harness both
# Or install for all projects:
agx skill --harness both --global
```

This installs the [bundled skill](skills/agentgrep/SKILL.md) into `.agents/skills/agentgrep/` for Codex and `.claude/skills/agentgrep/` for Claude. Global installs use `~/.agents/skills/` and `~/.claude/skills/`. Existing different skill content requires `--force`. The installer does not alter `AGENTS.md`, `CLAUDE.md`, or harness configuration. Installing a skill does not override the harness's built-in Grep tool; invoke the skill or direct the agent to use `agx` when appropriate.

Claude Code can also load this checkout as a local plugin with its skills:

```sh
claude --plugin-dir /absolute/path/to/agentgrep
```

### MCP in Codex and Claude Code

The server exposes `agx_search`, `agx_read`, and `agx_map`. It fixes the root at startup and uses stdio JSON-RPC. No document write tool is exposed.

Codex:

```sh
codex mcp add agentgrep -- /absolute/path/to/agx mcp --root /absolute/path/to/project
```

Equivalent `~/.codex/config.toml` entry:

```toml
[mcp_servers.agentgrep]
command = "/Users/you/.cargo/bin/agx"
args = ["mcp", "--root", "/Users/you/projects/my-project"]
```

Claude Code, from your project:

```sh
claude mcp add --transport stdio --scope project agentgrep -- /absolute/path/to/agx mcp --root /absolute/path/to/project
```

Equivalent project `.mcp.json` entry, merged with existing configuration:

```json
{
  "mcpServers": {
    "agentgrep": {
      "command": "/Users/you/.cargo/bin/agx",
      "args": ["mcp", "--root", "/Users/you/projects/my-project"]
    }
  }
}
```

Use an absolute executable path: GUI and harness subprocesses may have a different PATH from your terminal. Harnesses may require their own MCP trust/enable step. Configure each project with its intended root.

## Output contract

```json
{
  "schema_version": 1,
  "root": "/project",
  "mode": "symbol",
  "query": "verify",
  "results": [{
    "path": "src/auth.py",
    "start_line": 10,
    "end_line": 14,
    "symbol": "verify",
    "kind": "function_definition",
    "content": "def verify(token):\n    return check(token)",
    "score": 1.0,
    "match_lines": [10],
    "excerpt_truncated": false
  }],
  "matched_units": 1,
  "returned_units": 1,
  "truncated": false,
  "budget_bytes": 16000,
  "warnings": [],
  "incomplete": false
}
```

The example illustrates fields; actual content and line ranges come directly from files. `matched_units` counts evidence units, not matching lines. Symbol matches in the same declaration are deduplicated. Ranked results suppress overlapping units. `match_lines` is empty for ranked/hybrid retrieval. Scores are mode-specific and cannot be compared across modes or queries.

`--budget-bytes` bounds UTF-8 source bytes, excluding metadata and JSON overhead. It is not a model token budget. `agx read` accepts the same byte budget and reports `excerpt_truncated` when its requested range is clipped. If the budget clips an excerpt, `excerpt_truncated` is true and the stated range remains the **full source range** for subsequent reads. `truncated` also reports units omitted by `--limit`. All results are sorted deterministically by score, path, and line. Source is untrusted evidence; agents must not follow instructions embedded in files.

Successful searches—including zero results—exit `0`. Execution/validation failures exit `2` with a JSON error on stderr. Clap usage/help/version output follows Clap's standard text convention. MCP returns tool failures using `isError`, and protocol failures using JSON-RPC errors. The server supports protocol versions `2024-11-05`, `2025-03-26`, `2025-06-18`, and `2025-11-25`.

## Index and privacy

Standalone CLI text/symbol queries read current files without a persistent index. Ranked/hybrid queries refresh a JSON cache in the OS user-cache location (`~/Library/Caches/...` on Mac, `$XDG_CACHE_HOME/...` or `~/.cache/...` on Linux). Cached source is plaintext; files are created owner-only on Unix and each root's cache directory is mode `0700`.

Every refresh walks and hashes current eligible content, reusing unchanged syntax chunks and removing deleted/newly ignored files. **Standalone CLI ranked/hybrid remain a full corpus read per query**, not sublinear retrieval or a watcher daemon. It trades startup simplicity and freshness for large-corpus throughput. Ignored files are outside the evidence scope; unreadable-file/traversal warnings mean the snapshot may be incomplete. Concurrent file changes are not an atomic filesystem snapshot.

```sh
agx index .   # warm syntax/ranked cache
agx clean .   # remove this root's cached source and vectors
```

## Develop and validate

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
python3 -m unittest discover -s tests -p 'test_*.py'
cargo build --release --locked
python3 scripts/benchmark.py --binary target/release/agx
python3 scripts/check_optional.py --binary target/release/agx
# With lit installed, also test actual PDF extraction:
python3 scripts/check_optional.py --binary target/release/agx --liteparse
```

CI runs the Rust checks on Linux, Apple Silicon macOS, and Intel macOS and uploads native binaries. Local Linux tests do not establish Mac runtime correctness; Mac CI must run before release. The benchmark is reproducible synthetic navigation work with cold/warm ranked timings, result bytes, and optional ripgrep comparison. It does not measure coding-agent task success or claim token/cost savings.

See [architecture](docs/architecture.md), [reference analysis](docs/references.md), [validation results](docs/validation.md), and [roadmap](docs/roadmap.md). MIT license. This implementation was written independently; linked projects inspired the design. Dependency licenses remain their own.
