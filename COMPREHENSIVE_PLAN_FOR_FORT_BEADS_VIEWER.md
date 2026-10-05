# COMPREHENSIVE PLAN — FORT BEADS VIEWER (Go → Rust)

> **FORT** = **FOR**ward **T**o Rust. Plan to port `Dicklesworthstone/beads_viewer` (Go, ~120k LOC non-test) to Rust — not as a clone, but as a **Compatible Rust Successor**.
> Source: direct audit of repo @ `9ace029` (v0.20.0, Go 1.25), cloned at `./beads_viewer/`.
> Created: 2026-08-22. **Rev 4:** full English rewrite; two-track model (v1 Parity → v2 Native); TUI by user journey; upstream sync policy; core-vs-extended feature classification; TOON corpus; CP-CORR; agent map.
>
> **Goal statement:** keep everything the existing `bv` ecosystem depends on byte-compatible (robot JSON, CLI behavior, exit codes, data_hash, issue semantics, export formats) — then use Rust to build a better engine underneath: cleaner crates, lower memory, incremental analysis, stronger agent API. **Never mix the two tracks in one change.**

```
beads_viewer (Go)
        │  identical contract
        ▼
FORT v1 (Rust) ── drop-in replacement, differential-tested against Go goldens
        │
        ▼
FORT v2 (Rust-native) ── incremental engine, explainable scoring, richer agent API
                          (all additive, backward-compatible)
```

---

## 0. Executive Summary

`bv` is a graph-aware triage engine for the Beads issue tracker: reads `.beads/issues.jsonl` (or SQLite), builds the dependency DAG, computes 9+ graph metrics (PageRank, Betweenness, HITS, Eigenvector, Critical Path, Cycles, k-core, Articulation, Slack), and serves them through an **interactive TUI** and **~41 robot JSON commands** for AI agents.

**Measured port scope:**

| Component | Go LOC (non-test) | Notes |
|---|---|---|
| `pkg/ui` | 34,145 | bubbletea TUI — largest |
| `cmd/bv` | ~13,500 | main.go 10,603 + robot_registry.go 3,358 |
| `pkg/analysis` | 15,840 | graph algorithms + triage + cache |
| `pkg/correlation` | 10,921 | git↔bead correlation engine |
| `pkg/export` | 9,639 | Markdown/HTML/SQLite/Pages export |
| `bv-graph-wasm` (already Rust!) | 6,940 | **already Rust — reuse nearly wholesale** |
| Remaining ~18 pkgs | ~12,000 | loader, search, watcher, workspace, updater... |
| Test suite | ~90k LOC + 345 e2e test funcs | becomes our acceptance harness |

**Strategic finding #1:** `bv-graph-wasm/src/algorithms/*.rs` already implements, in Rust: PageRank (damp/tol config), HITS, eigenvector, betweenness exact+approx (`recommend_sample_size` with the same tiers as Go!), k-core, articulation+bridges, Tarjan SCC, cycle enumeration + cycle-break suggestions, critical path, slack, topo sort, reachability, subgraph extraction, what-if cascade, top-k set, parallel-cut. **≈70% of the graph engine work is extracting existing code into a shared crate, not writing new code.**

**Strategic finding #2:** Gonum supplies only `topo.Sort`, `topo.TarjanSCC`, `network.Betweenness`, `network.HITS(tol=1e-3)` in Go; PageRank/Eigenvector/k-core are hand-rolled there → no hidden library semantics to reverse-engineer beyond those four.

**Overall strategy:** Cargo workspace, bottom-up layered port (model → loader → analysis → correlation → robot CLI → export → TUI → search), keeping **byte-compatible output contracts** verified against frozen Go goldens. The verbatim copy of `bv-graph-wasm` already sits in `crates/`.

---

## 1. Current architecture (verified ground truth)

```
.beads/{issues,beads}.jsonl / beads.db(SQLite) / worktrees
        │
        ▼
internal/datasource ── best-source selection (priority SQLite=100 > worktree=80 > local=50,
        │              freshest mtime first; fused validate-load; LoadReport → load_stats)
        ▼
pkg/loader ──── discovery chain + tolerant parse (BOM strip, 10MB lines, skip malformed)
        │       GitLoader (--as-of, SHA-keyed cache, TTL 5min)
        ▼
analysis.Analyzer ── Phase1 sync (degree/topo/density)
        │            Phase2 async per-metric timeout goroutines
        │            Disk cache v2 columnar (BV_ROBOT=1 && !BV_NO_CACHE)
        ▼
   ┌────┴────────────────────────────────────────┐
   ▼                                             ▼
Robot CLI (~41 flags, RobotEnvelope JSON/TOON)   TUI (bubbletea Elm arch, 22 focus states)
   │                                             ├── background snapshot worker
   ├─ correlation (batched git log/cat-file,     ├── board/tree/graph/insights/history/
   │    5 cache layers, confidence scoring)      │   flow-matrix/attention/sprint/tutorial...
   ├─ search (semantic vector + hybrid scorer)   └── live reload (fsnotify/polling)
   ├─ export (md/html/sqlite/pages/wasm viewer)
   └─ workspace (.bv/workspace.yaml multi-repo)
```

This layering is preserved exactly — the port maps Go layers onto Rust crates without inventing a different dataflow.

---

## 2. Target Stack (Rust) — crates verified via docs.rs/lib.rs (2026-08)

| Go | Rust | Decision notes |
|---|---|---|
| bubbletea + bubbles + lipgloss | **ratatui 0.30** + crossterm | hand-written Elm loop over enum state; widgets replace bubbles; huh has NO equivalent → hand-build wizard forms |
| glamour (markdown in terminal) | **tui-markdown 0.3** (fallback: pulldown-cmark + own Span renderer) | no syntax-highlight blocks → add syntect only if parity requires |
| gonum/graph | **petgraph 0.3x** (tarjan_scc, articulation_points, bridges) + hand-implemented Kahn topological sort (sorted frontier for determinism) + reuse of wasm-crate algorithms | petgraph toposort order is unspecified → must reproduce Kahn+sorted-queue determinism |
| modernc.org/sqlite (pure-Go, FTS5) | **rusqlite 0.40 `bundled`** | FTS5 cleared: libsqlite3-sys build.rs compiles with `-DSQLITE_ENABLE_FTS5` unconditionally. Risk: bundled needs a C toolchain when cross-compiling CI |
| fsnotify | **notify 8.x** (+ PollWatcher fallback for NFS/SMB/SSHFS/FUSE) | event model differs (EventKind, rename cookies) |
| stdlib flag | **clap 4.x builder mode** | `#[arg(env)]` maps BV_* env vars naturally |
| goccy/go-json | **serde_json** (borrowed `&[u8]` deserialization for JSONL) | sonic-rs unnecessary |
| atotto/clipboard | **arboard 3.x** | Linux caveat: `SetExtLinux::wait()` or long-lived Clipboard instance |
| sahilm/fuzzy | **nucleo-matcher** (batch) / nucleo (search-as-you-type) | fuzzy-matcher(skim) legacy; helix moved off it |
| pgregory.net/rapid | **proptest 1.x** | strategy-based fits custom graph generators |
| yaml.v3 | **serde_yaml_ng** (or serde_norway). **serde_yml BANNED** — RUSTSEC-2025-0068 unsound/unmaintained | enforce via cargo-deny deny.toml |
| net/http client | **ureq 3.x** (rustls, blocking-first) | updater + GitHub API + Cloudflare checks |
| net/http server (preview!) | **tiny_http + hand-written SSE** (no axum/tokio — see §6.1) | preview.go serves static files + `/__preview__/events` SSE livereload — a frequently-forgotten server-side requirement |
| wasm-bindgen | keep **bv-graph-wasm** (verbatim copy in-tree) | proven pins: wasm-bindgen 0.2, js-sys 0.3, serde-wasm-bindgen 0.6, getrandom 0.4(wasm_js) |
| time.Time RFC3339(Nano) | **jiff 0.2** (pin minor until 1.0) or chrono 0.4 | jiff parses/prints faster; pre-1.0 semver risk noted |
| mattn/go-runewidth | unicode-width | |
| sbinet/gg + svgo (PNG/SVG render) | SVG: string templates (no crate needed); PNG: resvg/tiny-skia | only used by --export-graph png/svg |
| toon-go (TOON encoder) | **hand-written TOON encoder (~100–200 LOC)** | no Rust crate exists; must be byte-stable vs Go output |
| lipgloss colorprofile/theme detect | **terminal-colorsaurus** (OSC 10/11) or terminal-light | default dark palette when probe fails |
| x/sync errgroup | scoped threads / rayon (parallel parse sites) | |

### Workspace layout

```
beads_graph/
├── Cargo.toml                # [workspace]
├── crates/
│   ├── bv-core/              # model, loader, datasource (I/O + types — no algorithms)
│   ├── bv-analysis/          # graph, metrics, cache (split from core: faster builds,
│   │                         #   test isolation, cleaner wasm dependency)
│   ├── bv-correlation/       # git correlation engine (5 cache layers)
│   ├── bv-search/            # semantic index + hybrid scorer  [compatibility port]
│   ├── bv-export/            # markdown/graph/dot/mermaid/html/sqlite/pages
│   ├── bv-robot/             # robot command registry + envelope + TOON encoder
│   ├── bv-tui/               # ratatui app (views, worker, keybindings)
│   ├── bv-graph-wasm/        # COPIED VERBATIM (§2.1) — untouched until post-parity
│   └── bv/                   # binary: clap CLI, startup flow, dispatch
├── tests/                    # integration + e2e (golden JSON vs Go outputs)
├── bench/                    # criterion benchmarks (load/analysis/triage/export/tui-startup)
├── golden/                   # frozen Go robot outputs as oracle (incl. golden/toon/)
└── docs/
```

### 2.1 Workspace state (done)

`crates/bv-graph-wasm/` = **verbatim copy** (byte-identical, `diff -r` verified) of
`beads_viewer/bv-graph-wasm/` @ `9ace029` — original Cargo.toml, Makefile, README, src/
(6,940 LOC Rust: DiGraph + 13 algorithm modules + whatif/reachability/subgraph), tests/.

- **Nothing modified.** Extracting a pure-Rust `bv-graph-core` (no wasm-bindgen) plus a thin wasm wrapper is **post-parity (Phase 9) work**, NOT now — avoids maintaining two variants while Go upstream still updates algorithms.
- Provenance recorded in `PROVENANCE.md`. When Go upstream changes bv-graph-wasm: re-copy + re-diff is enough (the crate is self-contained, zero deps outside the wasm-bindgen family).
- Rule: this crate is a **read-only seed** until Phase 2 begins importing from it.

### 2.2 Feature classification (scope discipline)

Every ported feature is classified up front so "extended" items can never block a release:

| Class | Items | Release impact |
|---|---|---|
| **Core parity** (must ship in v1) | loader+datasource, analysis metrics, triage/plan/priority/suggest/alerts/graph/diff, history+correlation, markdown/HTML/sqlite/graph exports, static bundle generation, TUI core journeys (§6 Phase 6), recipes, drift/baseline, workspace | blocks CP-F |
| **Extended** (port after core parity passes; never blocks) | GH Pages wizard, Cloudflare/wrangler deploy, hooks ecosystem, watch-export, cass modal, updater self-update flow, sprint dashboard polish, semantic search wiring | may ship in v1.x patches |
| **Rust-native additions** (FORT v2 only) | incremental analysis, explainable scoring fields, streaming robot mode, capabilities declaration, typed IDs internally, mmap/binary caches | strictly additive, behind additive-only schema changes |

Search is explicitly tagged **compatibility port**: same presets/weights/index format as Go, with Rust-native cleanup deferred to v2 — semantic search drifts easily, so its behavior is pinned to goldens like everything else.

---

## 3. Compatibility contract (must not break)

These are the things that break the existing `bv` agent ecosystem if they diverge:

1. **RobotEnvelope**: `{generated_at(RFC3339), data_hash, output_format?, version?, load_stats?}` — `load_stats` emitted ONLY when the loader dropped records (`errors > 0`).
2. **data_hash** (`ComputeDataHash`): sha256 over issues sorted by ID, fields joined with `\0`: ID/title/description/notes/design/AC/assignee/source_repo/external_ref/status/type/priority/estimated_minutes/created_at/updated_at/closed_at (UTC RFC3339Nano)/labels(sorted)/deps(sorted by dependsOn,type,createdAt,createdBy)/comments(sorted); empty input → `"empty"`. **Byte-identical required.**
3. **MetricStatus** enum: `computed|approx|timeout|skipped|pending|panic` + elapsed ms + sample size when approx.
4. **Count semantics (#165)**: `open_count` = status exactly `open`; partition invariant `not_closed == actionable + not_actionable`.
5. **Exit codes**: drift check 0=OK / 1=critical / 2=warning; usage errors = 2; general errors = 1.
6. **TOON format** for `--format toon` (own encoder, validated against a dedicated golden corpus — see §5).
7. **`.bvvi` index format** (magic "BVVI", v1, LE) + `.bv/semantic/index-<provider>-<dim>.bvvi` path convention.
8. **Cache file formats**: decision — NO cross-binary compat needed; rename files (`analysis_cache.json` → `bv_analysis_cache_v3.bin`) so the Go binary never misreads Rust caches. Keep the *design* (keying, bounds, eviction), not the bytes.
9. **AGENTS.md blurb markers** `<!-- bv-agent-instructions-v3 -->` — inject/update/remove must recognize markers written by the Go version.
10. **CLI surface**: 142 flag registrations, ~41 robot primaries, modifier-requires rules (~50), 37 exclusive primary groups, argv rewriting (`bv triage` → `bv --robot-triage`; single-dash fix; bare `--json` → `--robot-triage`).

### 3.1 API Contract Freeze (Phase 0.5 — mandatory before upper layers are written)

Before Phase 1 starts, freeze the *internal* contracts (distinct from §3, which is the *external* output contract). These are cross-crate contracts so robot/TUI layers don't churn when core changes mid-flight:

| Contract | Frozen content |
|---|---|
| `RobotEnvelope` struct | shared serde struct owned by bv-robot; field order == serialization order |
| `Issue` / `Dependency` / `Comment` model | bv-core::model — serde attrs, Option semantics, custom deser points (§4.1) |
| `data_hash(issues) -> String` | signature + algorithm from item 2 above; fixed test vectors |
| `AnalysisConfig` / `GraphStats` / `MetricStatus` | bv-analysis public API; robot reads through access traits |
| Error model | `BvError` thiserror enum in crate-core; upper crates map to their own; NO anyhow across crate boundaries |
| Logging/tracing | tracing with target convention `bv::<crate>`; stderr-only in robot mode |
| Env/flag names | BV_* table (§4.6) — single source of truth in `docs/env.md`, asserted by a test |

Freeze = commit tag `api-freeze-v1`; any later change edits this plan + records a CHANGELOG "API change" entry. During parity, only *additive* changes allowed (new variant / new optional field).

### 3.2 Go upstream sync policy

The Go repo is very active (frequent commits and releases). Unmanaged drift would silently invalidate goldens:

- **During parity (until CP-F):** freeze on audited commit `9ace029`. No automatic syncing of Go source. Goldens are captured once from this commit and treated as immutable truth.
- **After v0.21.0-rust ships:** monthly upstream audit — diff Go changes since last audit, cherry-pick compatible fixes (especially bugfixes in algorithms/loader), regenerate affected goldens from the new Go binary, and record decisions in CHANGELOG. Any behavioral divergence found in our port vs newer Go gets triaged: fix forward (preferred) or document intentional deviation.
- The verbatim `crates/bv-graph-wasm` follows the cheap path: re-copy + re-diff whenever its upstream counterpart changes.

---

## 4. Subsystem breakdown & detailed port spec

### 4.1 `bv-core::model` (from `pkg/model/types.go`)
- `Issue` struct: 25 fields, exact JSON tags (`acceptance_criteria`, `issue_type`, `compaction_level`, ...). Optional fields → `Option<T>`; `Comment.ID` string-tolerant (accepts legacy numbers — custom `Deserialize` stringifies preserving textual form).
- `Status`: 10 values open/in_progress/blocked/deferred/draft/pinned/hooked/review/closed/tombstone.
- `IssueType`: any non-empty value is valid (Gastown extensibility); known types kept separately for sorting/icons: epic/feature/bug/task/chore/docs/question.
- `DependencyType`: blocks/related/parent-child/discovered-from; **both `""` and `blocks` are blocking** (legacy compat — easy to lose in an enum refactor; use `#[default]` + custom deser).
- Dependency deser accepts `depends_on_id` | `depends_on` | `target_id`.
- `Validate()`: ID+Title non-empty, Status ∈ enum, **reject `updated_at < created_at`** (feeds load_stats).
- Internally (v1): plain `String` IDs to guarantee deser/hash parity. Newtype `IssueId` etc. arrive in v2 behind the same serde shape.

### 4.2 `bv-core::loader` + `datasource`
- Discovery: `BEADS_DB` env (file→parent dir; ext .jsonl/.db/.sqlite/.sqlite3) > `BEADS_DIR` > `<repo>/.beads` > main-repo .beads (worktree resolution via `git rev-parse --path-format=absolute --git-common-dir`).
- `.beads/redirect` follow: max 4096 bytes, max depth 10; loop/non-UTF8 = ERROR; target must be a dir named `.beads`/`_beads`.
- File pick: candidates `*.jsonl` excluding `*.backup|.orig|.merge|deletions.jsonl|beads.left*|beads.right*` (merge artifacts → warning); preferred order `[issues.jsonl, beads.jsonl, beads.base.jsonl]`; bd/Dolt workspaces (detect `.beads/dolt/`|`embeddeddolt/`|metadata backend=="dolt") never fall back to stray non-issue JSONL.
- Parse: BOM strip line 1; 10MB/line buffer (`BV_MAX_LINE_SIZE_MB`); skip malformed + warn; `_type` records: memory/sprint/forecast/burndown → silent skip; unknown `_type` → skipped.
- Parallel parse ≥ 4MiB AND ≥512 lines, 64KiB×3 chunks/worker, warnings replayed in global line order — implement with rayon, deterministic results.
- Validation gates: MaxJSONLErrorRate 0.10; reject Valid==0 && Errors+Skipped>0; tombstone filter after load; validation cache keyed path+mtime+size.
- **SQLite reader** (`rusqlite`, read-only DSN): issues/dependencies/comments/labels tables (column/table presence probing), replacing modernc.
- `LoadReport { path, valid, errors, skipped, warnings(≤10) }` → process-global LAST_REPORT feeds envelope.load_stats.

### 4.3 `bv-analysis` — graph + cache (from `pkg/analysis`; algorithm seed = bv-graph-wasm copy)

**Size tiers (`ConfigForSize`) — copied verbatim:**

| Tier | nodes | Betweenness | PR/HITS/Cycles timeouts | MaxCycles | HITS condition |
|---|---|---|---|---|---|
| Small | <100 | exact, 2s | 2s | 1000 | always |
| Medium | <500 | exact, 500ms | 500ms | 100 | always |
| Large | <2000 | approx if density<0.01 (sample=`RecommendSampleSize`) else skip | 300ms | 50 | always |
| XL | ≥2000 | approx, 500ms | PR 200ms | 10 | only if density<0.001 |

`RecommendSampleSize`: <100 → n (exact); <500 → max(50, n/5); <2000 → 100; else 200.
Env overrides: `BV_SKIP_PHASE2` (disables PR/BW/HITS/cycles/eigenvector/critical-path; KEEPS k-core/articulation/slack), `BV_PHASE2_TIMEOUT_S`.

**Algorithms (numerically faithful ports):**
- PageRank: power iteration damp=0.85, tol=1e-6, maxIter=1000, deterministic.
- Betweenness: Brandes exact; approx Fisher-Yates seed=1, scale n/k.
- HITS: tol 1e-3 (gonum `network.HITS` semantics — replicate iteration/convergence behavior).
- Eigenvector: fixed 50 power iterations (no convergence check!).
- k-core: Batagelj–Zaveršnik; articulation: Tarjan undirected; slack = longest − fwd − bwd DP over topo; cycles: Tarjan SCC pre-check + one DFS cycle per SCC, capped + sorted.
- Critical path: topo DP depth; plan union-find: path compression + smaller-root-wins merge (determinism).

**Parity-first rule for this crate:** during parity, `bv-analysis` imports the algorithm modules directly from the verbatim `bv-graph-wasm` copy (module paths & feature gates preserved; only the wasm-bindgen dependency is severed at the boundary). NO restructuring into `bv-graph-core` before CP-F — every refactor of the graph layer adds a variable while the Go oracle comparison is still the safety net. The clean core/wrapper split happens in **Phase 9**, after parity, when upstream mirroring pressure ends. Only permitted adjustments: matching Go numerics listed above.

**Two-phase analyzer:** Phase1 synchronous; Phase2 = one task per metric with timeout channel-select (`std::thread` + `recv_timeout` mirroring Go's goroutine+select; panic→timeout status caught via `catch_unwind` at thread boundary); `AllPhase2Disabled` short-circuit.

**Cache (keep design, replace format):**
- In-proc TTL 5min global; key `dataHash16|configHash16`.
- Disk: gated `BV_ROBOT=1 && BV_NO_CACHE!=1`; dir `$BV_CACHE_DIR` else UserCacheDir/bv; 24h age, 10 entries LRU, 10MB entry cap; `.beads` mtime staleness check; XFetch beta=1.0 probabilistic early refresh; columnar SoA v2 shape.
- File locking: fs2/fd-lock (unix flock + Windows LockFileEx equivalent); no-rewrite-on-pure-hit; persist-only-fresh (never persist after git failure).

**Scoring constants (copied):**
- Impact: PR .22, BW .20, BlockerRatio .13, Staleness .05, Priority .10, TimeToImpact .10, Urgency .10, Risk .10 (risk = fan .30 + churn .30 + crossrepo .20 + status .20).
- scoreToPriority: ≥.7→P0, ≥.5→P1, ≥.3→P2, ≥.15→P3 else P4.
- TriageScore = base×0.70 + unblockBoost(≤0.15, threshold 5 unblocks) + quickWin(≤0.15, depth≤2); claimed-by-other ×0.1; QuickWin = log2(unblocks+1)×.4 + simplicity×.4 + prioBonus×.2.
- Priority rec thresholds: HighPageRank>.30, HighBetweenness>.50, staleness 14d, minConfidence .30, significantDelta .15.

### 4.4 `bv-search` — compatibility port
- Hash embedder: FNV-1a 64-bit signed buckets, default dim 384 (`BV_SEMANTIC_DIM`), L2-normalize — ~60 LOC, keeps `.bvvi` format.
- Vector index sync: batch 32, sha256 content-hash skip.
- Hybrid scorer weights (presets preserved):

| Preset | text | pagerank | status | impact | priority | recency |
|---|---|---|---|---|---|---|
| default | .40 | .20 | .15 | .10 | .10 | .05 |
| bug-hunting | .30 | .15 | .15 | .15 | .20 | .05 |
| sprint-planning | .30 | .20 | .25 | .15 | .05 | .05 |
| impact-first | .25 | .30 | .10 | .20 | .10 | .05 |
| text-only | 1.0 | 0 | 0 | 0 | 0 | 0 |

- Normalizers: status open=1.0→tombstone=0.0; priority P0=1.0→P4=0.2; recency exp(−days/30). Short-query boost .35, text floor .55, candidate floors 200/300. Env: BV_SEARCH_MODE/PRESET/WEIGHTS.
- FTS5 lives only in sqlite_export (issues_fts external-content porter unicode61) — belongs to bv-export.
- Deliberately scheduled LAST (Phase 7) and classified compatibility-port: not a parity blocker, easy to drift.

### 4.5 `bv-correlation` (heaviest git I/O subsystem)
- Extraction dispatch: followed-blob size ≥ 64KiB → snapshot path (`git log --raw --follow` + one long-lived `git cat-file --batch`, LRU memory window by lastUse); else legacy `git log -p --unified=0 --follow`. Snapshot path must produce event diffs **byte-identical** to legacy (recordLineSet = 64-bit hash line-multiset of lines starting '{' — equivalent hashing fine since only compared in-process... BUT diff output must match).
- Events: created/claimed/closed/reopened/modified; claimed = →in_progress (reopened if from closed/tombstone); within-commit events sorted by bead ID.
- Co-commit: 27 code extensions, excluded prefixes [.beads/, .bv/, .git/, node_modules/, vendor/, __pycache__/, .venv/, venv/, dist/, build/, .next/] via `:(exclude,glob)`; TWO batch passes `git log --no-walk=unsorted --name-status|--numstat`; rename "{old => new}" parsing; only claimed/closed events correlate.
- Explicit ID: builtin regex set + `--id-pattern` process-global registry; numeric-only normalized → `bv-N`; confidence base 0.90 (+0.05 closes/fixes/resolves, +0.02 bracket, +0.01 refs, +0.03 bead-, −0.02/extra-ID), clamp [0.70, 0.99].
- Temporal: author quoted-regex, claim→close window; base 0.50 ± concurrency/window/keyword-hint adjustments; clamp [0.20, 0.85].
- Co-commit base 0.95 (+0.04 msg contains ID, −0.10 >20 files shotgun, −0.05 all-test-files).
- MethodRanges: co_committed [.85,.99], explicit_id [.70,.99], temporal_author [.20,.85]; CombineConfidence = max + headroom×0.1×score per extra signal, cap 0.99; levels ≥.90 very high / ≥.75 high / ≥.50 moderate / ≥.30 low.
- 5 disk caches (report / HEAD-artifact / per-commit-event / per-commit-cocommit / analysis) — bounds: report ≤6 entries 24h 64MB; per-commit ≤4000 commits 30d 96MB, namespace primaryFile+"\0"+beadID.
- Feedback store: `<beadsDir>/correlation_feedback.jsonl` append-only, key (sha,beadID) last-write-wins.
- Incremental: threshold 100 new commits; `git rev-list --reverse sha..HEAD`.
- Timeout threading: every git call takes a context deadline (triage prologue ≤10s, env `BV_ROBOT_HISTORY_TIMEOUT_MS`) — Rust: `Command` + kill_on_drop + wait-with-deadline.
- Orphan detection signal weights: fix/close/resolve=10, implement=8, add=5, `#\d+`=15, `[a-z]{2,5}-\d+`=20, bv-x=25, beads?\d+=25.
- Related-work defaults: MinRelevance 20, MaxResults 10, ConcurrencyWindow 7d.

### 4.6 `bv-robot` (CLI contract layer)
- clap builder-mode (not derive — registry is dynamic): 142 flags, modifier-requires validation table, exclusive groups (37), argv rewriter (aliases `triage|next|plan|insights...`, single-dash normalize, `--json` rewrite).
- Startup flow in strict order (28 steps — pipeline documented in §6 notes).
- Envelope + per-command top-level schemas (triage/next/plan/priority/insights/alerts/suggest/graph/search/diff/drift/history/orphans/file-beads/hotspots/impact/file-relations/related/blocker-chain/impact-network/causality/sprint/forecast/capacity/burndown/label-trio...) — serde serialization with stable field order.
- TOON encoder hand-written: key folding, indent options, stats estimate; embedded directly (Go shells out to external `tru`; we remove that runtime dependency), keeping env knobs TOON_KEY_FOLDING/TOON_INDENT.
- Dead-code note: several phase-3 commands exist twice in Go (registry handler + unreachable inline block in main.go) — port exactly one path, the registry.

### 4.7 `bv-tui` (ratatui)
- Goal is **behavioral parity**: same navigation, shortcuts, workflows, state transitions, UX expectations. Rust-native improvements stay under the hood (render architecture, snapshot model) and never change default UX.
- 22 focus states + boolean view flags; View() priority chain (quit-confirm > modals > overlays > full views > split > list/detail) → match-arm ordering in draw fn.
- KeyRegistry: focus-keyed binding map + category metadata for help/sidebar — same design, HashMap<Focus, Vec<Binding>>.
- Combo keys (gg/G, 200ms combo window) — crossterm poll timeout.
- Layout: SplitViewThreshold=100, WideViewThreshold=140 (UltraWide=180 unused — dropped); applyContentSizing single sizing path; sidebar width 34 + gap 2; splitPaneRatio [0.2,0.8] step .05.
- Background worker (std::thread + crossbeam-channel):
  - Constants: debounce 200ms (`BV_DEBOUNCE_MS`), buffer 8, heartbeat 5s, watchdog 10s, heartbeat-timeout 30s, freshness warn 30s/stale 120s, poll tick 120ms, idle GC enabled.
  - Pipeline phases → snapshot pointer-swap (ArcSwap<DataSnapshot>), stale-drop by DataHash, coalescing, watchdog self-heal recovery counter.
- Views: list (virtualized windowing), detail viewport (markdown renderer), board (swimlanes status/priority/type, empty-collapse, 4-line cards, red/yellow/green/default borders), tree (parent-child only, root detection, cycle-safe DFS, children sorted priority→type→created), graph (2D rune-grid canvas + manhattan routing + topo layering — keep grid approach on ratatui Buffer), insights (6 panels + proof panel + heatmap), history (responsive <100/100-160/>160 panes, bead/git/file modes, timeline density, confidence cycling), flow matrix, attention, label dashboard, sprint dashboard (half-wired in Go — decision: port fully), alerts panel.
- Theme: Dracula palette adaptive light/dark, semantic colors (Theme.Blocked...), precedence --theme > BV_THEME > config.yaml > autodetect (terminal-colorsaurus).
- Mouse: wheel scroll per-focus, left-click row select via geometry inversion (listChromeLines helper ported).

### 4.8 `bv-export`
Core parity class: markdown report, mermaid, dot, interactive HTML, sqlite export, static bundle, preview server.
Extended class (never blocks release): GH Pages wizard, Cloudflare deploy, hooks, watch-export.

- Markdown report: sort open-first/priority/date; summary table; quick-actions bash (shellEscape); TOC slugs ([^a-z0-9]+→'-', dup -N); mermaid classDef open #50FA7B / inprogress #8BE9FD / blocked #FF5555 / closed #6272A4; sanitizeMermaidID; FNV-32a suffix on ID collision; edges ==> blocking / -.-> related.
- robot-graph json/dot/mermaid + BFS both-directions root/depth extraction (depth 0 unlimited).
- Interactive HTML: embed force-graph.min.js + marked.min.js; payload nodes(links/triage/metrics); filename convention `{proj}_graph_export__as_of__date__time__git_head_hash__<short>.html`.
- Static site bundle: index.html + viewer js + styles.css + coi-serviceworker.js + vendor/* (tailwind, alpine, dompurify, marked, mermaid, force-graph, d3.v7, chart.umd, sql-wasm, bv_graph.js+.wasm, fonts) + beads.sqlite3 (+config.json SHA256 OPFS key; %05d.bin 1MiB chunks if >5MB) + data/{meta,triage,project_health,graph_layout,history}.json + README + _headers + .github/workflows/static.yml template.
- SQLite export schema v1 (issues/dependencies/comments/issue_metrics/triage_recommendations/export_meta/issue_overview_mv/issues_fts porter unicode61; Optimize: journal_mode=DELETE, page_size=1024 httpvfs, ANALYZE, VACUUM) — rusqlite executescript.
- Preview server: tiny_http, bind 127.0.0.1, port scan 9000-9100, no-cache headers, safePreviewDir anti-traversal, SSE livereload (notify watcher 200ms debounce, root+1 level), script injected before </body> (Content-Length dropped, buffered until </html>).
- GH Pages wizard (extended): saved config ~/.config/bv/pages-wizard.json; steps export→target(github|cloudflare|local)→prereqs(gh auth/git identity | wrangler login)→preview→deploy (gh repo create/push --force-with-lease, pages enable 409-ok, rate-limit fallback legacy branch push, verify meta.json poll 5s≤90s); CF: wrangler pages deploy --commit-dirty=true, auth env>config>whoami 10s, URL *.pages.dev verify 30s.
- Hooks (extended): .bv/hooks.yaml (pre-export fail-fast default / post-export continue), Hook{name,command,timeout(default 30s),env(${VAR} expand),on_error}; env BV_EXPORT_PATH/FORMAT, BV_ISSUE_COUNT, BV_TIMESTAMP.
- Watch-export (extended): adaptive backoff 500ms→30s, skip via canonical content+dep hash.

### 4.9 Support subsystems
- **updater** (extended): GitHub releases/latest GET 2s timeout (ureq), UA header, 403/429 silent, semver compare w/ prerelease+dev-labels, asset bv_<os>_<arch>, checksums.txt sha256 verify, download cap 512MB, extract→smoke→backup-rename→install chmod 0755, rollback from .backup; TUI ⭐ indicator async.
- **agents blurb**: AGENTS.md > CLAUDE.md; marker v3; atomic writes; prefs ~/.config/bv/agent-prompts/<sha256(path)[:8]>.json.
- **cass** (extended): detector health (unknown/not-installed/needs-index/healthy, 5min cache, 2s timeout); searcher wraps `cass search --robot`; V-key modal.
- **instance lock**: `.beads/.bv.lock` O_EXCL, LockInfo{pid,started_at,hostname,owner_id}, stale takeover signal-0 liveness, release-only-if-owner — libc/nix + fs4.
- **drift/baseline**: .bv/drift.yaml thresholds (density 50%/20%, node/edge growth 25%, blocked +5, actionable ±30/20%, pagerank 50%, stale 14w/30c days, in_progress ×0.5, cascade info≥3/warn≥5, label_overrides); baseline.json v1; alert types new_cycle(critical)/density_growth/node_count_change/edge_count_change/blocked_increase/actionable_change/pagerank_change/stale_issue/blocking_cascade; cycleKey min-first rotation.
- **recipes**: builtin(embedded recipes.yaml, 11 names) < user(~/.config/bv/recipes.yaml) < project(.bv/recipes.yaml); schema filters{status,priority,tags,exclude_tags,created/updated_after/before Nd/Nw/Nm/Ny|ISO,has_blockers,actionable,title_contains,id_prefix} + sort(secondary recursive) + view + export + metrics[].
- **workspace**: .bv/workspace.yaml schema (repos{name,path,prefix default lower(name)-,beads_path,enabled}, discovery patterns ["*","packages/*","apps/*","services/*","libs/*","modules/*"] exclude[node_modules,vendor,.git,dist,build,target] max_depth 2); parallel load limit 32; prefix namespacing idempotent QualifyID; dep qualification local-if-no-known-prefix; --repo filter case-insensitive separators -:_ .
- **watcher**: notify + polling fallback; remote-FS detection (darwin Statfs fstypename nfs/smbfs/fuse; linux magic 0x6969/0xFF534D42/0x65735546 + mountinfo sshfs; windows DRIVE_REMOTE); debounce 250ms seq-guarded; poll 2s mtime+size; watches FILE'S DIRECTORY + basename filter (atomic-rename reliability); changeCh buffered(1) non-blocking send.
- **metrics/debug**: BV_METRICS=0 disables; BV_DEBUG stderr logger — Rust: tracing (stderr subscriber, env-filter).

---

## 5. Testing & Verification strategy

1. **Golden oracle**: build Go binary from the frozen commit, run the full robot-command matrix on dataset classes: this real repo (.beads 1.7MB sync_base), synthetic graphs (small chain / medium tree / large cyclic 600 nodes / XL 2500 nodes), edge cases (empty file, all-non-issue, BOM, malformed mix, bd/Dolt stub). Freeze outputs → `golden/`.
2. **Differential testing**: harness runs Go `bv` and Rust `bvr` side-by-side, jq-normalizes timestamps/timings/elapsed-ms, diffs. Byte-equal except time fields. This is the acceptance gate for every phase.
3. **Property tests** (proptest): random DAG generation → invariant checks (partition not_closed==actionable+not_actionable; data_hash stability under reorder; cache hit determinism; union-find track determinism).
4. **Invariance suite**: port the Go `invariance_test.go` properties — same-input-different-order produces same output.
5. **E2E**: port the important subset of the 345 Go e2e funcs (robot outputs, board render via debug-render, export bundle structures, drift exit codes).
6. **Snapshot renders**: debug-render equivalent (view → string) golden-tests insights/board at fixed size 180×50.
7. **UX Golden Checklist (TUI)** — because TUI can't be JSON-diffed, verify by user journey:
   ```
   Open app        → same default screen
   Press j/k       → same movement semantics
   Press /         → same search flow
   Open issue      → same detail information
   Switch b/g/E/i  → same navigation model
   Reload/watch    → same refresh behavior
   Quit/esc flows  → same confirmations
   ```
   Manual checklist (~40 items) executed at each TUI milestone; debug-render goldens automate what they can.
8. **CI**: cargo fmt/clippy/test + differential job (installs Go toolchain, builds Go bv) + cross-compile matrix (mac arm64, linux x86_64/arm64, windows) — bundled sqlite needs per-target cc (cargo-zigbuild or cross-rs).
9. **Fuzz (cargo-fuzz)** — the loader is the most dangerous surface, fuzz it before anything else:
   `parse_jsonl` (malformed/BOM/huge-line/legacy schema/number-comment-ID), `Dependency` deser (`depends_on`|`target_id` variants), redirect parser (depth loops), TOON decoder round-trip, recipe YAML deser. Corpus seeded from Go testdata/. Nightly CI, 5 min/target. Graph invariants belong to proptest; loader robustness belongs to cargo-fuzz.
10. **TOON golden corpus** (`golden/toon/triage.toon`, `insights.toon`, `graph.toon`, `history.toon`, `plan.toon`, `next.toon`...) captured from the Go binary with `--format toon` across every fixture class. TOON is where agent ecosystems fail hardest: assert both byte-equality AND decode-round-trip (TOON → JSON vs golden JSON). Every encoder change re-runs the whole corpus.

---

## 6. Roadmap — two tracks

> Original estimate (11–14 weeks) was optimistic. Realistic: **1 dev full-time ≈ 16–20 weeks**;
> with 2 agents coding genuinely in parallel (crate-split enables 3∥4∥5) ≈ **13–16 weeks**.
> Phase 6 TUI is the longest and most-underestimated phase — budgeted 5–8 weeks.
> **Track separation rule:** no Rust-native upgrades while chasing parity on sensitive layers. Port like-for-like → golden pass → optimize/upgrade → keep backward-compatible mode.

**FORT v1 = Phases 0–8 (parity, drop-in replacement).**
**FORT v2 = Phases 9–10 (native upgrades, additive only).**

### Checkpoints (verification milestones, not phases)

Each checkpoint = green differential tests + commit tag; a checkpoint opens the next phase:

| CP | Content | Gate |
|---|---|---|
| CP-A | bv-core loads same issues | issue count + field-level diff == Go on all fixtures |
| CP-B | data_hash identical | hash byte-equal on all fixtures incl. edge cases |
| CP-C | robot triage identical | --robot-triage diff (timestamps normalized) == 0 |
| CP-CORR | correlation parity (split from CP-D) | --robot-history on real repo: event count, method_distribution, confidence histogram (0.1 buckets), cache hit/miss ratio match Go; cold/warm fetch counts match Go tests |
| CP-D | full robot parity | all ~41 commands differential-pass |
| CP-E | TUI usable | smoke checklist 40 items pass on this very repo |
| CP-F | v0.21-rust parity release | full sweep + packaging |

### Phase 0 — Scaffolding + API Freeze (1 week)
- Workspace setup (§2 tree), CI skeleton, golden capture scripts (build Go bv, generate goldens incl. `golden/toon/`), fixture generators (synthetic JSONL graphs).
- **Phase 0.5 — API Contract Freeze (§3.1):** define + tag `api-freeze-v1` internal structs (Issue/RobotEnvelope/data_hash/GraphStats/BvError/tracing convention) BEFORE upper layers exist.
- Smoke-test rusqlite bundled cross-compile now (risk #3 answered early).
- **Deliverable**: green `cargo test` on clean shell; populated `golden/`; api-freeze-v1 tagged; cross-compile verdict on bundled sqlite (zigbuild/cross or native cc).

### Phase 1 — bv-core: model + loader + datasource (2 weeks)
- Types, tolerant JSONL parser (serial + rayon parallel), discovery chain, redirect follow, bd detect, LoadReport, validation gates, SQLite reader (rusqlite), GitLoader --as-of (SHA resolve, date layouts, TTL cache).
- Loader fuzz targets stand up here (cargo-fuzz: parse_jsonl, Dependency deser, redirect parser — §5.9).
- **Gate = CP-B**: data_hash == Go on every fixture; load_stats identical; as-of loads identical.

### Phase 2 — bv-analysis: graph + cache (imports wasm-copy code) (2–3 weeks)
- **Parity first, pretty later:** import algorithm modules directly from the verbatim `bv-graph-wasm` copy (module paths/feature gates intact; sever only the wasm-bindgen dependency at the boundary). NO bv-graph-core restructuring in this phase — that is Phase 9, after CP-F. Permitted adjustments limited to Go-semantics alignment (HITS 1e-3 gonum-style, eigenvector fixed-50-iter, cycle enumerate order, slack DP, Kahn sorted-frontier toposort).
- Two-phase analyzer + per-metric timeout + MetricStatus; disk cache v3 (design parity, new format+filename); ConfigForSize + env overrides; triage/plan/priority scoring constants (§4.3).
- **Gate**: differential robot-insights metrics equal (tolerance 1e-12); status flags identical; cache hit/miss behavior identical (timing ignored). **CP-C lands once Phase 3 wires triage.**

### Phase 3 — bv-robot: CLI + envelope + commands (2 weeks, ∥ second half of Phase 4)
- Full clap surface + modifier-requires validation + argv rewriter; envelope; TOON encoder; stable-field-order JSON serializer.
- **Dedicated TOON golden corpus** (`golden/toon/…`) — capture from Go `--format toon` across fixture classes; test byte-equal + decode-round-trip; encoder changes always re-run the corpus.
- Command order by value: triage family → plan → insights → priority → suggest → alerts → graph → diff → recipes/schema/docs/capabilities/help/metrics → label trio → drift/baseline → history/correlation family → sprint/forecast/capacity/burndown. (Search deliberately last — Phase 7.)
- **Gate = CP-D once Phase 4 done**: each command differential-passes goldens; exit codes; TOON round-trip.

### Phase 4 — bv-correlation (2–3 weeks, ∥ second half of Phase 3)
- Extractor snapshot+legacy paths, cat-file --batch streamer, co-commit batching, explicit/temporal scorers, 5 caches, feedback store, incremental extraction, orphan/file-index/network/causality/related.
- **Gate = CP-CORR** (standalone checkpoint, split from CP-D for independent debugging — correlation is the most dangerous git-I/O subsystem): on the real repo + synthetic git fixtures, --robot-history matches Go on four metrics: event count, method_distribution, confidence histogram (0.1 buckets), cache hit/miss ratio; plus cold/warm fetch counts matching Go tests. History/causality/network/related commands count toward CP-D only after CP-CORR is green.

### Phase 5 — bv-export (2 weeks, ∥ Phase 4)
- Core-parity scope: markdown/mermaid/dot; interactive HTML; sqlite export + FTS5; static bundle generation; preview server.
- Extended scope (after core parity green; never blocks): GH/CF deploy flows (ureq + gh/wrangler subprocess), hooks, watch-export backoff.
- Preview server: **tiny_http + hand-written SSE** (no axum/tokio — §6.1); upgrade later only if routing complexity demands.
- **Gate**: bundle structure diff; preview server smoke (status endpoint, SSE connect); sqlite schema introspection equal.

### Phase 6 — bv-tui (5–8 weeks — longest phase; port by USER JOURNEY, not by component)
Component-by-component porting risks three months without a usable app. Each milestone is a usable product increment:

- **TUI-M1 (weeks 1–2) — core journey**: open app → load repo → list issues → detail pane → filter/search → sort → quit. App shell (ratatui event loop), theme, KeyRegistry, list+detail+split layout, filter/sort. *Interim gate: usable as a simple viewer.*
- **TUI-M2 (weeks 3–4) — structural views**: board (swimlanes/cards/borders), tree, graph canvas (rune-grid + manhattan routing), actionable plan, label dashboard.
- **TUI-M3 (weeks 5–6) — analytics views**: insights 6-panel + proof, flow matrix, attention, alerts panel; history 3-pane responsive + file mode + timeline; time-travel; recipe/repo/label pickers.
- **TUI-M4 (week 7) — infrastructure**: background worker (thread + crossbeam, heartbeat/watchdog/self-heal, ArcSwap snapshot pointer-swap), live reload (notify/polling fallback), freshness indicators.
- **TUI-M5 (week 8) — chrome + polish buffer**: tutorial (include_str content), help overlay, shortcuts sidebar, modals (cass/update/agent-prompt), sprint dashboard, mouse handling; resize edge cases, combo-key timing, debug-render goldens, keyboard-map parity audit (every key in README §Keyboard Control Map), 10k-issue render perf pass.
- **Gate = CP-E**: debug-render snapshots structurally equal; manual smoke checklist 40 items; live reload works on this very repo.
- **Anti-death-spiral:** if TUI exceeds 8 weeks, ship CP-D robot-parity immediately as CLI-only `bvr` (agents ecosystem served right away); TUI continues on a branch. The robot layer is never hostage to the TUI.

### Phase 7 — bv-search + workspace + support systems (1–1.5 weeks, deliberately LAST)
- Search scheduled after TUI on purpose: semantic/hybrid search is NOT a parity blocker — meanwhile users keep the Go binary for search needs. Global priority order: loader → hash → analysis → robot → export → TUI → search.
- Includes: hash embedder + .bvvi index, hybrid scorer + presets, TUI semantic-mode wiring, workspace multi-repo, updater, agents blurb, cass, instance lock, drift/baseline leftovers.

### Phase 8 — Hardening + release (1.5 weeks) → **FORT v1 complete**
- Full differential sweep, proptest burn, overnight fuzz, race/loom where concurrent, packaging (brew tap formula, scoop manifest, install.sh/.ps1, nix flake, cargo-dist), docs (README + CHANGELOG), cut **v0.21.0-rust** (= CP-F).

### Phase 9 — Rust optimization (post-parity, isolated) → start of **FORT v2**
- Begins only after CP-F. Goal: retire compatibility scaffolding once Go-mirroring pressure ends.
- Items: extract `bv-graph-core` (pure-Rust algorithms) + thin wasm wrapper from the copied crate (the refactor deliberately deferred until now — every pre-parity refactor added risk); mmap/binary caches; rayon tuning; zero-copy loader (bytes::Buf instead of String); leaner error model; delete Go-shaped shims; newtype IDs (IssueId etc.) behind unchanged serde shapes; benchmark-gate every step (§6.2).

### Phase 10 — Rust-native features (additive only)
- **Incremental analysis**: changed nodes → affected subgraph → metric updates, leveraging the existing dataHash/cache/LRU foundation. Killer feature a pure port cannot offer.
- **Explainable scoring**: optional `reasons[]` fields on recommendations ("blocks 14 issues", "high pagerank", "stale 20 days") — additive, breaks nothing.
- **Richer agent API**: `capabilities: ["incremental-analysis", "streaming", "explain-score"]` declared in envelope/version metadata; streaming robot mode for large outputs.
- **Fast mode alongside compatibility mode**: compatibility mode = identical output/behavior (default); fast mode env/opt-in = parallel loader, incremental graph, zero-copy parsing, mmap caches.
- Plugin architecture exploration (only after the above land).

### 6.1 Runtime architecture decision (locked early — Option B)

**Sync core; async only at app-layer boundaries if ever.**
- Graph analysis is CPU-bound → std threads + rayon, NOT tokio.
- Phase-2 per-metric timeout: `std::thread` + channel `recv_timeout` (mirrors Go goroutine+select exactly; panic-catching via `catch_unwind` at thread boundary).
- TUI watcher/worker: thread + crossbeam-channel.
- HTTP client (updater/deploy checks): ureq blocking inside ordinary threads.
- tokio stays out of every crate's dependency tree. Benefits: fast builds, small binaries, no dual concurrency systems, direct translation of goroutine patterns.

### 6.2 Performance baseline & budget (bench/ criterion)

Go baselines measured via script on standardized hardware (M-series laptop, real repo + synthetic 10k):

| Bench | Go today (reference) | Target Rust |
|---|---|---|
| load 10k issues (JSONL) | ~295–910ms old full pipeline; warm triage ~90ms (v0.17) | ≤ Go p50 |
| analysis 5k nodes (Phase1+2) | timeout-bounded 500ms/metric | ≤ Go p50 |
| robot-triage warm cache | ~0.09s | ≤ 0.09s |
| robot-triage cold | ~2.3s | < 1.5s |
| export static bundle | no baseline yet → measure Phase 5 | ≤ Go |
| TUI startup → first frame | < 50ms Go spec | ≤ 50ms |
| RSS @ 10k issues | measured Phase 0 | ≤ 70% Go |

Rules: every PR from Phase 2 on runs `bench quick`; >10% regression blocks merge. Go baseline numbers frozen into `golden/perf_baseline.json` for fair same-machine comparison. v2 adds a second tier on top of parity targets: fast-mode wins (incremental refresh latency, RSS) tracked separately.

### 6.3 Multi-agent execution map (if run as Claude/Codex swarm)

Crate boundaries double as ownership boundaries — each agent owns its crates and talks through frozen contracts (§3.1); nobody edits another's crates:

| Agent | Owns | Phase | Coordination notes |
|---|---|---|---|
| A | bv-core (model/loader/datasource) | 1 | goes first; publishes api-freeze-v1 types |
| B | bv-analysis (graph/cache/scoring) | 2 | consumes A's types; imports from crates/bv-graph-wasm copy |
| C | bv-robot (CLI/envelope/TOON) | 3 | consumer of A+B; owns golden/toon corpus |
| D | bv-correlation | 4 | most independent; needs only A; separate CP-CORR gate |
| E | bv-export | 5 | needs A + B metrics; independent of D |
| F | bv-tui | 6 | joins after CP-D; consumes everything |

Anti-conflict rules:
- **Integration owner = Main session**: merges, runs differential sweeps, decides checkpoint tags.
- Contract changes go through the owner — NEVER direct edits to another agent's crate files.
- Golden files are shared read-only: only the owner's capture script writes them.
- If schedule squeezes: skip any Phase-2 restructuring (already default), and if TUI slips → ship CP-D CLI-only `bvr` first (risk #11).

---

## 7. Risks & mitigations

| # | Risk | Level | Mitigation |
|---|---|---|---|
| 1 | Float divergence Go↔Rust (map iteration order affecting sum order in PR/BW) | HIGH | all aggregations iterate deterministically (sorted node index); differential gate tolerance 1e-12 + sorted outputs; on failure mirror Go's exact accumulation loop order (read computePageRank/hits carefully) |
| 2 | petgraph toposort nondeterminism vs gonum | HIGH | hand-write Kahn with BTreeSet frontier; petgraph only for SCC/articulation/page_rank if semantics match, else keep wasm-crate ports |
| 3 | rusqlite bundled C toolchain in cross-compile CI | MED | cargo-zigbuild / cross-rs images include cc; smoke-test in Phase 0 |
| 4 | Hand-written TOON encoder format drift | MED | dedicated golden/toon corpus per payload type; TOON spec public (github.com/toon-format) |
| 5 | Git subprocess behavior differences (buffering, SIGPIPE, exit codes) | MED | uniform strategy: piped stdin/stdout, kill_on_drop, read-to-end; differential on real repos |
| 6 | TUI parity "feel" (combo keys, resize, mouse) | MED | debug-render goldens + UX golden checklist (§5.7); accept cosmetic diffs, block behavioral diffs |
| 7 | serde_yml trap | LOW (blocked) | banned via cargo-deny deny.toml |
| 8 | Scope creep porting dead code (main.go inline dup paths, UltraWide threshold, half-wired sprint) | LOW | pre-decided §4.6/§4.7: single registry path; sprint dashboard ported fully |
| 9 | jiff pre-1.0 churn | LOW | pin =0.2.x exactly; chrono-free wrapper module |
| 10 | Cache format drift causing stale reads across versions | LOW | renamed files + version magic; ignore-on-mismatch |
| 11 | TUI kills the project (longest phase, endless polish) | HIGH | hard cap 8 weeks; CP-D robot-parity ships standalone as CLI-only `bvr` if TUI slips — agents ecosystem served immediately; polish confined to week-7–8 buffer; TUI ported by user-journey milestones so something usable exists every ~2 weeks |
| 12 | bv-core bloat | LOW (handled) | bv-analysis split out (§2 tree); bv-core stays pure I/O + types |
| 13 | Optimistic timeline erodes trust midway | MED | 16–20 weeks solo / 13–16 weeks dual-agent; progress measured by checkpoints CP-A→F via differential tests, not vibes |
| 14 | Upstream Go drift invalidating goldens mid-port | MED | §3.2 sync policy: frozen SHA during parity, monthly audit after v1 |

---

## 8. Deliberately NOT ported (intentional trimming)

1. **UltraWideViewThreshold=180** — declared-unused in Go.
2. **Inline duplicate robot handlers in main.go** — the registry path is canonical.
3. **External `tru` dependency for TOON** — encoder embedded; runtime dependency removed.
4. **`ReadyTimeoutMsg`** — legacy no-op.
5. **CapsLockTracker** — exists in Go but unwired from Update; port only on demand, not a parity blocker.
6. **Old vendor JS** — vendored assets kept as-is (they are final products, not ported code).
7. **pgregory/rapid → proptest** — paradigm shift; port *properties*, not test-by-test.
8. **Go-shaped compatibility shims in Rust code** — live only until CP-F; Phase 9 cleans them.

---

## 9. Definition of Done (FORT v1)

- [ ] All checkpoints CP-A → CP-F pass (differential, self-claims don't count), including standalone **CP-CORR** for correlation. All ~41 robot commands differential-pass vs Go goldens on all 4 dataset classes. TOON golden corpus (`golden/toon/`) passes byte-equal + decode-round-trip.
- [ ] TUI opens this very repo; every view navigable; live reload works; background-worker indicators correct.
- [ ] Static site export opens offline; FTS5 search works; WASM graph what-if animation runs.
- [ ] Drift CI contract exit codes correct.
- [ ] Perf: bench/ meets §6.2 budget (RSS ≤ 70% Go, runtime ≤ Go p50 on headline benches).
- [ ] Fuzz: 5 loader targets nightly ≥ 1 week with no new crashes/malloc failures.
- [ ] Install paths: brew/scoop/install.sh/cargo binstall.
- [ ] AGENTS.md blurb injection recognizes markers written by the previous Go version.
- [ ] Docs: README + CHANGELOG document port origin + deviations (cache format, tru removal).

## 10. Definition of Done (FORT v2, preview)

- [ ] `bv-graph-core` extracted; wasm wrapper builds from it; browser viewer regression-passes.
- [ ] Incremental analysis: single-issue edit refreshes affected-subgraph metrics measurably faster than full recompute (benchmarked).
- [ ] Explainable scoring: optional reasons[] fields shipped; all v1 goldens still pass unchanged (additive-only verified).
- [ ] Capabilities metadata + streaming robot mode documented and consumed by at least one scripted agent workflow.
- [ ] Fast mode opt-in beats compatibility mode on §6.2 benches without changing default outputs.

---
*Basis: real-code audit, not speculation — every constant cites file:line in the cloned repo `./beads_viewer/`. Detailed scout reports live in session history for deeper lookups.*

## 11. Session status log (2026-09-03)

An audit against `./beads_viewer/` (Go, cloned fresh) found the crate graph
compiling and passing tests, but several real bugs and a large surface of
undispatched/stubbed robot commands and TUI views. This log tracks what's
been fixed vs what remains, so work is resumable across sessions instead of
silently re-discovered.

### Fixed this session (commits `18becee`, `5d4f3e1`)

- **bv-core**: `discover_repos` produced backslash-separated relative paths
  on Windows (`packages\web`), breaking multi-repo workspace discovery.
- **bv-tui**: instance-lock takeover shelled out to `kill -0` unconditionally
  — nonexistent on Windows, so liveness checks always failed and even our
  own just-acquired lock was reclaimed as stale. Added a real
  cross-platform check + short-circuit for our own PID.
- **bv-correlation**: `combine_confidence` diverged from Go's
  `CombineConfidence` (per-method clamping + normalized boost vs Go's raw,
  progressively-updated-headroom formula) — silently wrong correlation
  ranking for any multi-signal correlation. Rewritten to match exactly.
- **bv-tui**: Tree and Alerts views were wired to hardcoded empty slices —
  always rendered blank regardless of real data. Added a real parent-child
  tree builder (`views::tree::build_tree_nodes`, matches Go
  `pkg/ui/tree.go` semantics) and a real `App.alerts` vector.
- **bv CLI**: any `--robot-*` flag registered in the flag registry but not
  yet dispatched fell through to silently launching the interactive TUI —
  dangerous for a robot/agent/CI caller. Now fails fast, exit 2, clear
  message, instead.
- **bv-analysis** (new `label_health` module): `robot-label-health`,
  `robot-label-flow`, `robot-label-attention` were dispatched but returned
  **hardcoded empty JSON** — a fake "no data" success, worse than an error.
  Ported the real algorithms from `pkg/analysis/label_health.go`: label
  extraction/stats, velocity/freshness metrics, cross-label blocking-flow
  with bottleneck detection, composite per-label health, PageRank/
  betweenness-weighted attention ranking. One documented scope cut: attention
  scoring sums the already-computed *global* PageRank over a label's issues
  rather than re-running PageRank on an extracted per-label subgraph (Go's
  `ComputeLabelSubgraph`/`ComputeLabelPageRank` — not ported, see below).
- **bv-analysis** (new `blocker_chain` module) + **bv CLI**:
  `robot-blocker-chain <issue-id>` now real — ports
  `Analyzer.GetBlockerChain` (DFS over open blockers, direct + parent-child
  propagation, cycle detection) from `pkg/analysis/graph.go` +
  `triage_context.go`'s `OpenBlockers`/`IsActionable`. One approximation:
  `IsActionable` doesn't model Go's scheduler-deferral concept beyond
  `Status::Deferred` (no scheduler exists in this Rust port).
- **bv CLI**: `robot-priority`'s `--robot-by-label`/`--robot-by-assignee`
  modifiers now filter the issue set before scoring (were previously
  declared in the flag registry but silently ignored).
- **bv CLI**: `robot-confirm-correlation`/`robot-reject-correlation` now
  record real feedback via `bv-correlation::feedback::FeedbackStore`.
  Documented scope cut: Go cross-checks the SHA against that bead's actual
  correlation history via the correlator pipeline (not ported — see below)
  before recording and captures the correlation's original confidence;
  this records directly (bead-ID-validated only) with `original_conf: 0.0`
  and says so in `usage_hints`. Revisit once the correlator pipeline lands.
- **bv CLI**: `robot-capabilities`/`robot-schema`/`robot-metrics`/
  `robot-docs` now real (lower-fidelity — see each function's doc comment):
  capabilities/schema report actual per-command implementation status
  cross-checked against an explicit `DISPATCHED_ROBOT_COMMANDS` list
  (not guessed); metrics honestly reports empty timing/cache (no metrics
  subsystem exists — never fabricates numbers) plus real dataset size;
  docs returns a minimal real topic index. None replicate Go's large
  hand-authored per-command doc/schema text.
- **bv-correlation** (new `correlator` module) + **bv CLI**: a real,
  documented-simplified correlator pipeline now backs
  `robot-explain-correlation`, `robot-correlation-stats`,
  `robot-file-beads`, `robot-file-hotspots`, `robot-file-relations`.
  Walks full git log once (sha/author/message/files), scores each
  commit↔issue pair via existing `explicit.rs` (message ID mentions) and
  `temporal.rs` (same-author-in-active-window) primitives. Documented
  scope cuts vs Go's 882-line `correlator.go`: single git-log walk instead
  of two merged strategies, no incremental/cached-artifact layer, and the
  temporal signal requires an exact assignee↔git-author match (Go's is
  looser, weighted by concurrent active-bead count — more false negatives
  here, fewer false positives). 4 unit tests cover explicit-mention,
  assignee-gated temporal, out-of-window exclusion, and no-signal cases.
- **bv-search** (new `cosine_similarity` in `embedder.rs`) + **bv CLI**:
  `robot-search` now real. `--search-mode text` (Go default) ranks issues
  by cosine similarity of `hash_embed(title+description)` against the
  query; `--search-mode hybrid --search-preset NAME` blends that with
  `bv-search::hybrid`'s status/priority/recency components. Scope cut: no
  persisted vector index / incremental sync — embeds every issue fresh on
  each call (Go's `index.Sync`/`syncStats` not ported; `index`/`loaded`
  envelope fields intentionally omitted rather than faked). 3 new
  integration tests (limit respected, missing-query rejected by the
  shared modifier-requires validator, unknown-preset exits 2).
- **bv-correlation** (new modules `cocommit.rs`, `causality.rs`, `network.rs`):
  - `cocommit`: co-commit extraction + confidence scoring, reusing the
    correlator's git walk. 4 unit tests cover path-exclusion, bead-ID-
    mention boost, shotgun-commit penalty, and missing-sha skip.
  - `causality`: lifecycle causal chain for one bead from its
    `BeadEvent` list + gap/duration analysis + summary/recommendations.
    Documented scope cut: blocked-period/critical-path insights not computed
    (blocked transitions not present in `BeadEvent`). 5 unit tests.
  - `network`: full impact/relation network (shared-commit + shared-file +
    dependency edges) with BFS depth-limited sub-network extraction.
    Documented scope cut: cluster detection not ported. 5 unit tests.
- **bv CLI**: `--robot-causality <bead-id>`, `--robot-related <bead-id>`,
  `--robot-impact-network <bead-id|all>` now real, all backed by the new
  correlation modules.
- **bv-core** (new `sprint` module + `Sprint`/`Forecast`/`BurndownPoint`
  model types in `model.rs`) + **bv CLI**: `--robot-sprint-list`,
  `--robot-sprint-show <id>`, `--robot-burndown [--burndown-sprint <id>]`,
  `--robot-forecast [--forecast-sprint <id>]`,
  `--robot-capacity [--capacity-label <label>]` now real. Loads from
  `.beads/sprints.jsonl` (same JSONL format as Go). Documented scope cuts
  vs Go: burndown simplified (no per-day scope-change tracking), forecast
  velocity-target-only (Go includes graph-metric factors), capacity basic
  snapshot (Go simulates agent availability). 4 unit tests + 4 integration
  tests. 33 of 47 robot primaries now real.

### Confirmed remaining gaps (not fixed — do not assume otherwise)

**Robot CLI** — ~14 of 47 `--robot-*` primaries in `flags::ROBOT_PRIMARIES`
still have no dispatch handler at all; they correctly exit 2 via the A4
fallback instead of misbehaving, but the underlying commands don't exist
yet. Roughly in build order of what's cheapest to unblock:
- No new backing algorithm needed, just wiring: `robot-impact` (note: Go's
  is "impact of modifying **files**", not `bv_analysis::impact` which scores
  *issues* — don't wire these together without re-checking
  `handleRobotImpact` in `cmd/bv/robot_registry.go`), `robot-not-ready-labels`
  (needs `build_triage`'s claimability logic extended, not a simple filter —
  see `pkg/analysis/triage.go` `isClaimableRecommendation`/`buildTopPicks`).
  `robot-diff` needs `--diff-since` git-snapshot comparison logic — not
  audited yet.
- The remaining undispatched commands are `robot-docs` full-text help (only
  a minimal topic index exists — low value), `robot-help` variants, and
  some edge-case flag combinations.

**TUI** (`crates/bv-tui`, ~2,940 lines vs Go `pkg/ui`'s ~24,000+): entire
views missing or placeholder — Graph (`ViewMode::Graph` renders nothing,
falls through to List), History/time-travel (`ViewMode::TimeTravel` same),
Flow-Matrix, Attention, Sprint, real Tutorial (currently just re-renders the
`?` help overlay under a different title), label/recipe/repo pickers,
semantic search, velocity comparison, update/agent-prompt modals. No
keybinding customization (`pkg/ui/keybindings.go` has no Rust counterpart —
low priority).

**Correlation** (`crates/bv-correlation`, 10 files vs Go `pkg/correlation`'s
24): the major algorithms are now all ported — `correlator.rs` (explicit-ID
+ temporal-author signal assembly), `cocommit.rs` (co-commit confidence),
`causality.rs` (lifecycle causal chain + gap/duration analysis), `network.rs`
(shared-commit/file/dependency edge graph + depth-limited sub-network) plus
`extractor.rs`, `explicit.rs`, `temporal.rs`, `scorer.rs`, `feedback.rs`,
`orphan.rs`. Remaining unported correlation pieces are pure performance
optimization (`index_sync.go`, `incremental.go`) or the Go-specific
batched-git-plumbing (`cocommit.go`'s `primeBatch`/`batchLogArgs`) which
this port sidesteps by reusing a single `git log` walk.

**bv-search** (4 files vs Go `pkg/search`'s 14): `robot-search` works now
(text + hybrid modes, real cosine-similarity ranking) — but it embeds
fresh on every call. Vector index persistence/incremental sync, lexical
boost, and query adjustment (`index_sync.go`, `lexical_boost.go`,
`query_adjust.go`) are not ported; irrelevant until something needs
cross-call index caching for performance.

None of the above should be treated as "safe to assume implemented" —
verify against this list (or re-grep `flags::ROBOT_PRIMARIES` vs
`main.rs`'s dispatch chain) before relying on a command's output.

## 12. Session status log (2026-09-09)

### Fixed this session (commits `147bdef`, `4f1cf7d`, tag `v0.1.2`)

- **CI**: the `check` job's `actions/checkout@v4` had no `fetch-depth`,
  defaulting to a shallow (depth-1) clone. `cargo test --workspace` runs
  `crates/bv/tests/golden_comparison.rs`, which shells out to `git log`
  (directly, and via `bv-correlation`) for `--robot-history` and other
  git-derived robot commands — shallow history made those outputs diverge
  from the frozen goldens, pushing content-parity failures past the ratchet
  baseline (15 > 10) even though nothing had actually regressed. The
  `differential` job already had `fetch-depth: 0` for this exact reason;
  `check` didn't. Added it there too.
- **Release v0.1.2**: workspace version 0.1.1 → 0.1.2; `--version` now
  reads `env!("CARGO_PKG_VERSION")` instead of a hardcoded string (was
  drifting from `Cargo.toml` — the CLI printed `0.1.1` while the crate was
  already `0.1.1`/next bump would've silently gone stale again). Tagged
  and pushed; release workflow (multi-arch build + GitHub Release) runs
  from the tag push, unchanged from what's documented in this plan.

### Investigated, no fix needed — Go upstream has moved to v0.24.1

A parity check against a locally-installed `bv` binary surfaced apparent
regressions across nearly every robot command (`--robot-priority` schema,
`--robot-triage`/`--robot-insights` ordering, a new `authority_hash` field,
`Cycles: null` vs `[]`, `robot-alerts` dropping `labels`, `--robot-next`
behaving completely differently under `BV_TEST_MODE`). Investigation
(`git clone --depth 1 --branch v0.24.1`, diffed against the frozen
`9ace029`) found these are **not bugs in `bvr`** — the installed Go binary
had been `go install`ed at HEAD and was `v0.20.0`/`v0.24.1`, both **ahead**
of this repo's frozen parity target (§3.2: "freeze on audited commit
`9ace029`. No automatic syncing of Go source"). Re-verified every
"divergence" directly against the frozen `golden/*.json` files (not a live
Go binary) and confirmed `bvr`'s output matches the frozen target exactly
in every case checked — `Cycles: null` and no `labels` field are what the
*golden* files themselves contain, matching Rust, not the newer Go binary.

**Real scope of the Go→Go upstream drift** (measured, not estimated): full
repo diff `9ace029..v0.24.1` is 257 files / 49,363 insertions. Scoped to
just the packages `bvr` ports (`cmd/bv`, `pkg/robot`, `pkg/analysis`,
`pkg/correlation`) it's still 102 files / 17,844 insertions —
`cmd/bv/main.go` alone rewrote 3,576 lines; `pkg/analysis/priority.go`
changed algorithm (+293 lines, not just field renames — explains the
`robot-priority` schema change: `direction`/`explanation.status` replaced
`impact_score`/`issue_id`/`reasoning`); `pkg/analysis/cache.go` added an
entire new caching subsystem (+1,167 lines); `pkg/correlation/causality.go`
+748 lines.

**Decision** (per §3.2, reaffirmed, not changed): stay pinned to `9ace029`
for FORT v1. This is comparable in size to the *original* Go→Rust port
(§0's LOC table) — not a "quick sync", a second parity project. If/when
this repo is ready to chase v0.24.1, treat it as its own phase: audit the
real Go diff file-by-file (source, not binary output), port algorithm
changes deliberately (not cosmetic field renames), re-capture the entire
`golden/` corpus at the new commit, and re-run every differential gate.
Do not attempt to shortcut this by pattern-matching new Go binary output
into Rust — confirmed above that doing so produces changes that look
right but are algorithmically unverified.
