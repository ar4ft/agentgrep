# Integrating agentgrep with nain

**This change prepares agentgrep. It does not add a panel to current nain.** Nain currently has no extension API for custom sidebar panels or search providers. Installing agx, its agent skill, or the example adapter does not create “Code Search”. A separately authorized native change in [ar4ft/nainzed](https://github.com/ar4ft/nainzed) is required. No nain files are modified here.

Reviewed nain `main` commit: `ac4785f6dfcc2e6d4fd68d1cb98fdc99da9ebed2`. The reviewed nain README enforces no AI startup and removed telemetry. Its existing native [`workspace::dock::Panel`](https://github.com/ar4ft/nainzed/blob/main/crates/workspace/src/dock.rs) trait supports dock position, focus, rendering, icon and toggle action, but is not a public extension search-provider API. Use that native foundation rather than reintroducing an agent panel or AI provider registry.

## Install and executable discovery

```sh
git clone https://github.com/ar4ft/agentgrep.git
cd agentgrep
cargo install --path . --locked
agx --version
agx search 'session token' /path/to/project --mode ranked
agx serve --stdio --restricted
```

Cargo installs `agx` into `~/.cargo/bin` by default. GUI applications may have a different PATH from the terminal. The future native adapter should expose an optional setting such as `code_search.agx_path` containing an **absolute executable path**. That setting is a proposed nain interface, not an existing setting. An invalid explicit path should fail visibly, never silently select another executable.

Discovery order: explicit path, inherited PATH, `~/.cargo/bin/agx`, `~/.local/bin/agx`, `/opt/homebrew/bin/agx`, `/usr/local/bin/agx`. Check that the candidate is an executable file; canonicalize it. Do not download binaries, invoke a shell, install a model, run `doctor` (which probes optional dependencies), install an agent skill, configure MCP, or trigger `agx update` from the nain adapter. Installation and tool updates remain explicit user actions.

A compiled, framework-independent Rust subprocess example is provided in [`examples/nain_adapter.rs`](../examples/nain_adapter.rs):

```sh
cargo run --example nain_adapter -- /path/to/project 'needle' \
  --mode symbol --agx "$HOME/.cargo/bin/agx"
```

It discovers the executable, launches structured argv, negotiates the restricted protocol, indexes, validates session/schema, and prints results. It is a demonstration, not a drop-in nain extension. Its synchronous waiting belongs on a background task; a production adapter must multiplex responses, handle cancellation/restart/timeouts, and shut down gracefully off the UI thread. The example kills/reaps its subprocess on exit and sends diagnostics to local stderr only.

## Initial CLI adapter

Use `[agx_path, "search", query, canonical_root, "--mode", mode]`, with mode restricted by a native enum to text/symbol/ranked. Put options before `--` for queries starting with `-`. Parse the default JSON output and follow [CLI schema and error rules](cli-json.md). Attach the nain worktree/root ID, query generation and subprocess ID externally. Cancel by terminating superseded subprocesses. This path is usable independently of the worker, but searches disk rather than unsaved buffers and incurs traversal/startup on each request. Preserve nain's ordinary project search and existing magnifier action.

## Native panel and provider contract nain must add

Add a native `CodeSearchPanel` implementing `Panel`, `Focusable`, `EventEmitter<PanelEvent>` and `Render`, registered with the workspace's dock/panel lifecycle. It should have a stable persistent panel key, saved dock position/size, a focus handle for its query field, and a distinct `ToggleCodeSearch` action. Add a separate activation button **beside** the existing magnifier; do not replace or redirect ordinary project search.

Suggested UI: a dockable “Code Search” panel alongside Files and Outline; Symbol and Ranked tabs (optional Text); query input; file-glob and language filters; a keyboard-navigable result list; source/context preview; Enter opens at the exact range and focuses the editor. Show indexing state, cancellation, actionable worker errors, truncation and incomplete/skipped-source indicators. Label Ranked as “BM25 lexical ranking”. Excerpts are literal untrusted source text: display as text/code without executing Markdown links, shell snippets or instructions from results. No AI terminology, assistant actions, inference calls, telemetry, remote counters or usage reporting belong in this flow.

Create a local `CodeSearchProvider` interface independent of AI/agent/MCP registries. A suggested native contract:

```rust,ignore
trait CodeSearchProvider {
    // Background/async operations, with typed structures rather than stringly UI calls.
    fn open_workspace(&mut self, roots: &[WorkspaceRoot]) -> Task<Capabilities>;
    fn refresh(&mut self, root: RootId, force_hash_rescan: bool) -> Task<IndexStatus>;
    fn update_document(&mut self, root: RootId, path: RelativePath,
                       version: u64, full_content: String) -> Task<MutationAck>;
    fn saved_document(&mut self, root: RootId, path: RelativePath,
                      version: u64) -> Task<MutationAck>;
    fn close_document(&mut self, root: RootId, path: RelativePath,
                      version: u64) -> Task<MutationAck>;
    fn files_changed(&mut self, root: RootId, paths: &[RelativePath]) -> Task<IndexStatus>;
    fn ignores_changed(&mut self, root: RootId) -> Task<IndexStatus>;
    fn search(&mut self, request: LexicalSearchRequest) -> SearchTask;
    fn cancel(&mut self, request_id: RequestId);
    fn shutdown(&mut self);
}
```

This is an interface proposal, **not compiled nain API**. `LexicalSearchRequest.mode` must be an enum containing only Text, Symbol, Ranked; it must have no model/provider/URL fields. `SearchTask` carries the request ID and an async result plus a cancellation handle. Register this provider under ordinary project/worktree services, never model registries. Keep `is_agent_panel()` false and do not import agent, assistant, model-provider or telemetry crates. Preserve nain's existing production AI/telemetry audits; add checks for this provider separately when implementing nain.

## Persistent worker lifecycle

1. Spawn one local `agx serve --stdio --restricted` per workspace with piped stdin/stdout and bounded local stderr diagnostics. No shell; no extra service. Start lazily when Code Search is activated, and stop on workspace closure/provider disablement. Remote projects are out of scope; do not send local filesystem roots to a remote inference or hosted search service.
2. Negotiate [protocol v1](editor-protocol.md); validate `restricted:true`, result schema 2, supported modes, limits, and network/telemetry false. Assign stable workspace/root IDs from nain's worktrees. Refuse unsupported/mismatched workers instead of falling back to hybrid or MCP.
3. Index each root off the UI thread. Render correlated `index/progress`; do not block Files/Outline/project search. Root changes require a fresh worker session.
4. Map editor buffers to `(root_id, relative_path)`. Send full content on open/edits, with strictly increasing versions across reopening within a worker session. Debounce/coalesce edits; acknowledge the latest document update before issuing dependent searches. Buffers must fit negotiated limits; show a visible unavailable/limit state if not. Never spill unsaved content to temporary searchable disk files.
5. On successful save, call `document/save` with the acknowledged overlay version after disk writes complete. If a new edit occurs during save, do not retire that newer overlay: forward the old saved disk change and keep the newer version active. On close/discard call `document/close` with the exact current version. Versions must survive close/reopen until the worker restarts.
6. Forward worktree changes/deletions via `workspace/files_changed`. Ignore-rule changes use `workspace/ignore_changed`. For directory moves, watcher overflow, reactivation after missed changes, or untrustworthy timestamps, reconcile with refresh or hash rescan. No internal watcher means missed events can produce stale disk results until reconciliation.
7. Debounce queries, cancel superseded IDs, and retain a separate UI query generation. Search one root per request with its latest acknowledged `expected_index_version`; fan out across roots in the adapter. Do not treat BM25 scores from different roots as globally calibrated; group by root or provide a deliberate merge policy.
8. Before displaying/opening, compare request/UI generation, session ID, root identity, index version, overlay document version and current buffer identity. Discard stale results even if cancellation raced with success. For disk results, compare content hash when exact preview validity matters. Convert 1-based lines to zero-based editor points; convert UTF-8 byte columns if the editor API expects another encoding. Highlight the full evidence range, not an invented token match.
9. On `index_not_ready`, repair with refresh. On `stale_index`, reissue only if the query is still current. On `busy`, coalesce/back off; keep outstanding requests well below 32. On process death, stop using all old-session results, restart with backoff, renegotiate and replay current roots/buffers before searching. Never log entire source buffers or queries as diagnostics. User-visible local errors are sufficient; add no telemetry.

The worker does not change agx's standalone inference-capable tools. Restriction is enforced at the worker method/parameter surface and tested with a local Ollama listener plus a trap executable. The executable still includes standalone code; this integration is a restricted API, not a separate stripped binary or an OS network sandbox.

## Validation before a nain release

Agentgrep CI runs the real worker/CLI tests, adapter example checks, release build and cold/warm benchmark on native Apple Silicon (`macos-15`), Intel (`macos-15-intel`) and Linux. It does not establish nain GUI behavior. After the separate native nain change, test dock placement/button separation, keyboard/focus behavior, exact source opening, GUI PATH discovery, unsaved edits, save races, ignored files, multi-root identity, worker crash/restart and workspace closure on both Mac architectures. Audit the native production dependency tree and confirm no AI or telemetry calls are introduced.

Notebook/cell and virtual-document mapping is not implemented. Initial native provider scope should be ordinary local text files. An `.ipynb` file is currently searched as raw JSON/text, so its file-line ranges cannot be treated as notebook cell locations. Exclude notebooks from the panel until nain adds an explicit cell-to-source mapping; preserve nain's existing notebook search.
