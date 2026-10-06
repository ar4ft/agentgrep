# Editor worker protocol v1

Start the existing native executable with `agx serve --stdio --restricted`. `--restricted` is explicit but optional: this worker always exposes only text, symbol, and lexical BM25 ranked search. It is separate from MCP. No embeddings, Ollama, model downloads, inference, telemetry, external subprocesses, or update checks are invoked by its methods. Standalone `agx search --mode hybrid`, MCP, parsing, and release-update commands remain available outside this worker.

## Transport and negotiation

UTF-8, one JSON object per newline, over stdin/stdout. This is an agx-specific protocol, **not JSON-RPC or LSP**. Requests require `id`, `method`, and object `params`; `capabilities` may omit params. IDs are nonempty strings up to 128 UTF-8 bytes or unsigned 64-bit integers. Prefer strings in JavaScript clients to avoid integer precision loss. Active IDs must be unique; reuse only after the terminal response. JSON-escape embedded newlines in document contents. Do not prefix requests with `Content-Length`.

Every outgoing message has `protocol_version: 1`. A request gets exactly one terminal `{id,result}` or `{id,error}` response; progress notifications have `method`/`params` and no ID. Cancellation is the sole inbound notification. State-changing operations and searches run in arrival order on one actor; a separate input thread processes cancellation immediately. Queue depth is 32, in addition to one active operation; total accepted in-flight/queued encoded frames are capped at 8 MiB. A full queue returns `busy` rather than blocking cancellation behind more requests.

Protocol messages exclusively use stdout. Fatal diagnostics use stderr. Ordinary request errors remain on stdout and do not terminate the process. EOF drains accepted requests and exits; to discard a long pending operation, cancel it before EOF or terminate the subprocess. Broken output pipes exit cleanly. A fatal I/O/initialization failure outside request handling exits 2; successful worker shutdown exits 0. The client should bound incoming frames, handle process death, and restart explicitly.

First send:

```json
{"id":"init","method":"initialize","params":{"protocol_version":1,"workspace_id":"nain-project-42","restricted":true,"roots":[{"id":"main","path":"/Users/me/My Project"}]}}
```

A successful `result` includes:

```json
{"protocol_version":1,"result_schema_version":2,"workspace_id":"nain-project-42","session_id":"1234-1791300000000000000","restricted":true,"search_modes":["text","symbol","ranked"],"capabilities":{"cancellation":true,"document_overrides":true,"document_versions":true,"document_save":true,"file_notifications":true,"index_progress":true,"language_filters":true,"network":false,"telemetry":false,"hybrid":false,"models":false}}
```

This capability excerpt omits `roots`, `limits`, and `ranked_algorithm` for readability. Require protocol 1, result schema 2, restricted=true, and the three advertised modes. Initialization selects the immutable roots for this process; 1–8 unique IDs and unique canonical local directories are allowed. Reinitialization fails. Unsupported protocol versions return `unsupported_protocol`. `restricted:false` is rejected; it cannot turn this worker into a model-capable server. Configure a new process when roots change. `capabilities` with empty params returns the complete negotiation result again.

## Methods

| Method | Params | Behavior |
| --- | --- | --- |
| `initialize` | `protocol_version`, `workspace_id`, `roots:[{id,path}]`, optional `restricted`, `limits` | Configure roots; no indexing yet. |
| `capabilities` | `{}` or omitted | Return negotiated capabilities, limits, session and roots. |
| `index/refresh` | `root_id`, optional `expected_index_version` | Discover files and ignore changes; compare metadata, reuse unchanged source/parses/ranking terms; read changed files. |
| `index/rescan` | Same | Explicit reconciliation: reread/hash all eligible disk files, including changes with unchanged size/mtime. Overlays still win. |
| `index/status` | Same | Return readiness, index version, counts, memory accounting, and skipped files. |
| `workspace/ignore_changed` | Same | Refresh ignore membership and eligible documents, including unsaved new files. |
| `workspace/files_changed` | `root_id`, `paths:[relative_path]` | Force reread of these regular files; add new files and remove deleted files. At most 1024 paths. Expand directory events to file paths or request refresh. |
| `document/update` | `root_id`, `path`, `version`, `content` | Full UTF-8 replacement for an unsaved buffer, including not-yet-created files. No disk write. |
| `document/save` | `root_id`, `path`, `version` | After a successful disk save, retire that exact overlay and reread disk. The editor buffer may remain open. |
| `document/close` | Same | Retire that exact overlay on close/discard; restore disk content or remove an unsaved-only document. |
| `search` | Fields below | Search the cached root snapshot; never perform query-time disk I/O. |
| `cancel` | `request_id` | Best-effort cancellation of an active/queued request. Can be a notification; with its own ID, returns `{cancellation_requested:boolean}`. |

Every non-cancel method requires a request ID, including editor change events. Wait for mutation acknowledgements before accepting a dependent search. Unknown methods return `method_not_found`; hybrid/model/embedding, parse, doctor, MCP, and update operations have no worker entry points. Unknown params are rejected, including `model`, `embedding_model`, service URLs, and `hidden` in search.

Search params:

```json
{"id":"q-12","method":"search","params":{"root_id":"main","expected_index_version":3,"query":"session token","mode":"ranked","glob":["*.swift","!Tests/**"],"languages":["swift"],"limit":20,"budget_bytes":16000}}
```

`mode` defaults to `text`. Text and symbol use regex with smart case: uppercase query characters imply case-sensitive search. `literal:true` escapes regex metacharacters; `case_sensitive:true` overrides smart case; `context` defaults to 2 and permits 0–100. Symbol mode finds the smallest recognized declaration containing each matched source line, falling back to context lines. It searches declaration bodies as well as names; it is not a type-resolved symbol database.

Ranked mode is BM25 (`k1=1.2`, `b=0.75`) with identifier splitting, stop words, and symbol/path boosts. It does not infer synonyms or relationships. Regex/literal/case/context options do not alter ranked term scoring. Filters constrain candidates; worker BM25 statistics describe the whole configured root. CLI ranked statistics describe the CLI's filtered corpus, so filtered scores and some overlap/tie ordering may differ. Scores are relative relevance, not probabilities, and are not comparable across snapshots or modes.

Globs support positive includes and `!` exclusions, matched against relative paths; patterns without `/` match basenames in any directory. Supported language filters are `rust`, `python`, `typescript` (including TSX), `javascript` (including JSX), `go`, `swift`, and `text` for everything else. Languages and globs combine by intersection. Query size is capped at 4096 UTF-8 bytes, 64 globs of at most 4096 bytes each, and 16 language entries.

## Freshness and cancellation

Initialize, then index each root with `index/refresh`. Searches before indexing, or after an interrupted/failed indexing mutation, return `index_not_ready`. Index progress notifications report `request_id`, workspace/root identity, `phase` (`started`, `indexing`, `complete`), files seen/read/reused. Progress is emitted every 32 discovered source files and at start/completion; it is not a total-percentage estimate. Failed/cancelled requests get an explicit terminal error and may leave a partially refreshed cache; a successful refresh is required to make it queryable again. `index/status.incomplete` also reports such partial state. `ready` records whether a root has ever completed indexing; it alone does not mean a dirty snapshot is searchable.

Each root's `index_version` increases on mutations and refresh attempts, even if source is unchanged. Versions are meaningful only with `session_id`. Searches may specify `expected_index_version`; mismatch returns `stale_index` instead of results. An editor must discard responses from an old session, root mapping, query ID/UI generation, index version, or document version. Never use an old excerpt as the current buffer contents.

`document/update.version` is a strictly increasing editor-supplied integer per root/path. Equal/older updates return `stale_document`. Save/close requires the exact active overlay version; stale save/close cannot remove newer edits. Version watermarks remain after save/close for this worker session. A reopened buffer must keep increasing its version. Overlays take precedence over filesystem changes and deletions until explicitly saved/closed. Closed/disk documents return `document_version:null`; use their BLAKE3 `content_hash` and index version as freshness identifiers.

```json
{"id":"edit","method":"document/update","params":{"root_id":"main","path":"Sources/Session.swift","version":17,"content":"func sessionToken() {\n    print(\"unsaved\")\n}\n"}}
{"id":"edit","protocol_version":1,"result":{"root_id":"main","path":"Sources/Session.swift","document_version":17,"index_version":3,"indexed":true}}
{"method":"cancel","params":{"request_id":"q-12"}}
{"id":"q-12","protocol_version":1,"error":{"code":"cancelled","message":"cancelled"}}
```

Cancellation checks run during traversal, source preparation, tree-sitter parsing, syntax-node walking, chunk term construction, and searching. A filesystem call, regex evaluation on a bounded line, JSON parse/write, or a small sort may finish before the next check. Cancellation racing an already completed request may yield normal success or `cancellation_requested:false`; the editor must also reject superseded UI query IDs. Cancelling a search does not invalidate the index. Document replacements are prepared before committing; cancellation before commit preserves the previous overlay/version.

There is **no internal filesystem watcher** and no freshness claim for unreported disk changes. Nain must forward worktree events, saves, deletions and ignore changes. On watcher overflow, missed events, focus restoration, or external changes whose metadata cannot be trusted, use `index/rescan`. `index/refresh` scans metadata and ignore membership; equal-size/equal-mtime changes require a forced path notification or rescan. No worker cache is persisted to disk.

## Search result schema 2

The outer response contains protocol version and request ID. Its `result` retains CLI evidence fields and adds editor metadata:

| Field | Meaning |
| --- | --- |
| `schema_version` | `2` for worker search results; CLI stays `1`. |
| `workspace_id`, `root_id`, `root`, `session_id` | Client workspace identity, configured root identity, canonical root directory, unique worker session. |
| `index_version` | Root snapshot used throughout this result. |
| `mode`, `query`, `results` | Selected lexical search and ordered evidence. |
| `matched_units`, `returned_units` | Number after unit deduplication/overlap suppression and number actually returned. These are source units, not raw matching lines. |
| `budget_bytes` | Configured total UTF-8 excerpt budget; excludes metadata. |
| `truncated` | Some evidence omitted by unit/source/serialized response limits, or an excerpt clipped. |
| `response_truncated` | Complete serialized response limit required clipping/omission, including possible skipped-file sample omission. |
| `incomplete`, `warnings`, `skipped_files` | Source eligibility/read/resource omissions; distinct from result clipping. |

Each result has `workspace_id`, `root_id`, `index_version`, relative `path`, `start_line`, `end_line`, `symbol` (nullable), syntax/line/window `kind`, numeric `score`, `content` (the excerpt), `match_lines`, `excerpt_truncated`, `match_lines_truncated`, `source` (`disk` or `overlay`), nullable `document_version`, `content_hash`, `language`, and `source_range`.

Lines and `match_lines` are **1-based**. `start_line` and `end_line` are inclusive, even when the excerpt is clipped. `source_range.start` is `{line:start_line,byte_column:0}`; `source_range.end` is `{line:end_line,byte_column:<full final line's UTF-8 byte length>}` with `end_exclusive:true`. Columns are **0-based UTF-8 byte offsets**, not UTF-16 units or display cells. CRLF is normalized to LF in excerpts. Open at `(start_line-1,0)` in a zero-based editor; highlight the full source range using the current document, converting byte columns if required. Ranges cover source evidence/context, not a token-level exact-match span. Symbol may be null and kind may be `lines`/`window`; render these as text evidence rather than inventing a declaration name. Text/symbol scores are 1.0. Ranked `match_lines` is empty because term ranking is not a regex-match list. Per-unit match lines are capped at 256 with `match_lines_truncated:true`.

Example evidence object (identity/hash values shortened only in prose examples):

```json
{"workspace_id":"nain-project-42","root_id":"main","index_version":3,"path":"Sources/Session.swift","start_line":1,"end_line":3,"symbol":"sessionToken","kind":"function_declaration","score":1.0,"content":"func sessionToken() {\n    print(\"unsaved\")\n}","match_lines":[2],"excerpt_truncated":false,"match_lines_truncated":false,"source":"overlay","document_version":17,"content_hash":"a 64-character BLAKE3 hexadecimal digest","language":"swift","source_range":{"start":{"line":1,"byte_column":0},"end":{"line":3,"byte_column":1},"end_exclusive":true}}
```

`skipped_files` contains `total`, per-reason `counts`, up to 64 `{path,reason}` samples, and `samples_truncated`. Binary formats, NUL content, non-UTF8 and symlinks are excluded explicitly. File-size, memory, file-count, depth, read-race/read-error and syntax-declaration limits make a search incomplete. Ignore/hidden/build-directory exclusions are the configured search scope and are not enumerated as skipped files. Counts are observed omissions, not a count of all unvisited files after a traversal limit. Targeted updates conservatively retain earlier omission reports; refresh rebuilds them.

## Limits and filesystem scope

Optional initialization `limits` defaults:

```json
{"memory_bytes":67108864,"max_file_bytes":262144,"max_files":10000,"response_bytes":2097152}
```

Allowed ranges: memory 1–256 MiB, source file 1 KiB–2 MiB, files 1–100000 across roots, serialized search response 4 KiB–4 MiB. Search unit limit is 1–1000 and excerpt budget 1–1000000 bytes. Request frames are limited to 4 MiB including the newline. Large/malformed frames are drained and return an error; the worker stays available. Full replacement buffers must fit both frame and source limits; JSON escaping can enlarge a frame.

Retained source, syntax excerpts, line offsets, ranking term data, and overlays use conservative memory accounting. Source data and parse/ranking caches are evicted/skipped when admission would exceed the budget; dirty-buffer updates that do not fit are rejected instead of dropping acknowledged edits. Tree-sitter output is capped at 256 declarations and 2× the source-file byte limit per document; excess syntax is reported and text/windows remain available. Traversal depth is at most 64. Discovery path bookkeeping is bounded separately to one eighth of the configured memory budget. At most 64 omission samples are retained.

`memory_accounted_bytes` is **not process RSS or a hard OS memory limit**. One in-flight parse/read, parser/runtime/allocator overhead, request JSON, the bounded 32-frame/8 MiB encoded queue, transport serialization, and temporary per-file sorting use additional memory. Limit input frame sizes and outstanding editor requests in the adapter; for an OS-enforced process ceiling, the host must apply its own resource policy. Searches scan cached lines/chunks; BM25 has cached term frequencies/statistics but no global inverted candidate postings. This is an interactive foundation, not a large-monorepo scalability guarantee.

All paths supplied after initialization are root-relative normal paths; absolute paths, `..`, `.`, duplicate separators, symlink components and paths escaping the canonical root are rejected. Root-relative paths are capped at 4096 bytes. Hidden files, `.git`, `node_modules`, `target`, `.venv`, `vendor`, `dist`, `build`, `.agentgrep`, and unsupported binary formats are excluded. Workspace `.gitignore` and `.ignore` rules apply, including to new overlays; `.ignore` takes precedence. Rule files must be confined regular UTF-8 files <=64 KiB. Parent/global Git ignores and external `.git/info/exclude` are intentionally not read by the editor worker. An ignored parent directory cannot be re-included by a child rule.

Confinement checks reject ordinary path traversal and symlinks. They are not an OS sandbox against an attacker concurrently replacing filesystem ancestors between checks and reads; run only with the user's normal local workspace permissions. No paths outside configured roots are exposed by search methods. Rule-read failures either leave indexing dirty with an error, or omit a directory with an explicit read-error/incomplete report; they never follow ignore-file symlinks.

## Error codes

`parse_error`, `frame_too_large`, `invalid_request`, `duplicate_id`, `busy`, `invalid_params`, `unsupported_protocol`, `unsupported_mode`, `method_not_found`, `unknown_root`, `index_not_ready`, `stale_index`, `stale_document`, `cancelled`, `response_limit`. Errors are `{code,message}`; messages are descriptive, bounded, and not stable identifiers. Malformed/unidentified frames use `id:null`. Version mismatch, stale state, cancellation and limits are recoverable. Unknown/model params are `invalid_params`; forbidden modes are `unsupported_mode`. An absent initialization is `invalid_params` with “initialize first”. Do not classify errors by message text.
