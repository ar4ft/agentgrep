# Editor cold/warm benchmark

Run `python3 scripts/benchmark_worker.py --binary target/release/agx --files 1000`. Raw local results: [Linux x86_64](benchmarks/editor-linux-x86_64.json). Native Mac CI publishes the same report for Apple Silicon and Intel as workflow/release artifacts.

Corpus: 1,000 generated Python files, 60 lines per file, 1,687,800 source bytes. One process/index for the worker; five warm-query samples per mode. The worker uses a **256 MiB configured cache allowance**, above the 64 MiB default, so the entire corpus fits its conservative accounting. It indexed all 1,000 files without incomplete results. Measured cache accounting: 113,613,273 bytes; observed RSS at one snapshot: 61,348 KiB. Neither value is peak RSS.

| Operation | Local Linux time (ms) | Scope |
| --- | ---: | --- |
| Worker process + initialize | 1.637 | One sample |
| Worker cold index | 299.159 | One sample, 1,000 reads |
| Worker warm text | 3.875 | Median of 5, 8 results |
| Worker warm symbol | 4.793 | Median of 5, 8 results |
| Worker warm ranked BM25 | 11.933 | Median of 5, 8 results |
| Worker unchanged metadata refresh | 17.755 | One sample, zero source reads, 1,000 reused |
| Worker one-file update | 0.762 | One sample, one source read |
| CLI warm text | 13.581 | Median of 5, includes process startup |
| CLI warm symbol | 18.625 | Median of 5, includes process startup |
| CLI ranked first | 778.071 | One sample, new root/cache |
| CLI warm ranked | 134.020 | Median of 5, includes process startup/full source reads |

These are synthetic measurements, not equivalent-results speed guarantees, Mac performance, nain GUI responsiveness, or task-success/recall evaluations. Corpus generation may warm filesystem pages: “cold” means a new worker/cache, not a flushed OS cache. CLI schema 1 and worker schema 2 differ in metadata and filtered BM25 statistics/tie behavior; serialized byte sizes differ. Warm worker queries exclude initial indexing and do no freshness I/O, whereas CLI queries verify current files. The worker relies on editor file events/rescans to achieve freshness.

Remaining limits: linear cached scans, no global candidate postings, reparsing a changed document rather than incremental tree edits, synchronous ordered state operations, best-effort cancellation around bounded noninterruptible calls, no internal watcher, conservative admission that may omit a large corpus at defaults, and no hard OS RSS ceiling. Retained syntax duplicates some source. For real repositories, measure complete indexing, latency distributions, peak memory and sustained edits before increasing limits or claiming interactive scalability.
