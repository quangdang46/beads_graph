# AGENTS.md — beads_graph (bvr)

> Guidelines for AI coding agents working in this Rust codebase — a Compatible Rust Successor of [Dicklesworthstone/beads_viewer](https://github.com/Dicklesworthstone/beads_viewer) (Go).

---

## RULE 0 - THE FUNDAMENTAL OVERRIDE PREROGATIVE

If I tell you to do something, even if it goes against what follows below, YOU MUST LISTEN TO ME. I AM IN CHARGE, NOT YOU.

---

## RULE NUMBER 1: NO FILE DELETION

**YOU ARE NEVER ALLOWED TO DELETE A FILE WITHOUT EXPRESS PERMISSION.** Even a new file that you yourself created (test files included). You must always ask and receive clear, written permission before ever deleting a file or folder of any kind.

---

## Irreversible Git & Filesystem Actions — DO NOT EVER BREAK GLASS

1. **Absolutely forbidden without explicit user authorization in the same message:** `git reset --hard`, `git clean -fd`, `rm -rf`, force-push to `main`, or any command that can delete/overwrite code or data.
2. **No guessing:** if uncertain what a command might destroy, stop and ask.
3. **Safer alternatives first:** prefer `git status`, `git diff`, `git stash`, backups.
4. **Document authorization:** when running any approved destructive command, record the user's exact words and the command run.

---

## Git Branch: ONLY Use `main`

- All work happens on `main`. Commits go to `main` directly (solo project).
- Never reference `master` anywhere; if you see it, it's a bug.

---

## Toolchain: Rust (Cargo workspace)

- **Stable Rust** (edition 2021). Check with `rustc --version`.
- **Format before every commit:** `cargo fmt --all`
- **Lint:** `cargo clippy --workspace --all-targets -- -D warnings`
- **Test:** `cargo test --workspace`

### Key Commands

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

### Workspace Layout

| Crate | Owns |
|---|---|
| `crates/bv-core` | model types, JSONL loader, datasource discovery |
| `crates/bv-analysis` | graph algorithms, two-phase analyzer, cache, scoring |
| `crates/bv-correlation` | git↔bead correlation engine |
| `crates/bv-search` | semantic index + hybrid scorer |
| `crates/bv-export` | markdown/html/sqlite/pages export |
| `crates/bv-robot` | robot command registry + envelope + TOON encoder |
| `crates/bv-tui` | ratatui terminal UI |
| `crates/bv-graph-wasm` | **verbatim upstream copy — read-only seed until Phase 9** (see PROVENANCE.md) |
| `crates/bv` | binary crate |

### Critical Dependencies & Decisions

- **NO tokio anywhere.** Sync core: std threads + crossbeam-channel + rayon. Per-metric timeouts via `std::thread` + `recv_timeout` mirroring Go goroutine semantics. HTTP via blocking `ureq`.
- **petgraph toposort is nondeterministic** → hand-written Kahn sort with sorted frontier only.
- **serde_yml BANNED** (RUSTSEC-2025-0068). Use `serde_yaml_ng`.
- **rusqlite bundled** (FTS5 enabled unconditionally by libsqlite3-sys).

---

## The One Non-Negotiable: Compatibility Contract

Everything in this repo exists to produce a **drop-in replacement** for Go bv **v0.25.0**. These contracts are verified by differential testing against frozen Go goldens (`golden/`, captured from commit `18afafa`; see `golden/METADATA.txt`):

1. **Robot JSON schemas** — field-for-field identical, stable field order.
2. **`data_hash`** — byte-equal sha256 algorithm. Go rewrote this between v0.20.0 and v0.25.0: v0.20.0 hashed flat NUL-joined fields; v0.25.0 (`pkg/analysis/cache.go:167`) uses a fingerprint algorithm (`writeUintHash(len)` then `ID` + `ContentHash` + `DependencyHash` per issue) and widened the digest from 16 to 64 hex chars. **Ported in commit `3ff48bc`** — `crates/bv-core/src/fingerprint.rs` + `data_hash.rs` now implement the v0.25.0 form and are byte-exact against the Go oracle (`compute_data_hash_v020` is retained only for historical reproducibility). The data must also come from the **JSONL**, not `beads.db`: Go's `loader.LoadIssues` reads JSONL, and the SQLite reader omits fields (`source_repo`, `closed_at`, dependency `created_at`/`created_by`) that the hash covers.
3. **CLI surface** — same flags, modifier-requires validation, argv rewriting.
4. **Exit codes** — 0 success / 1 general+critical-drift / 2 usage+warning-drift.
5. **TOON format** — byte-stable vs golden corpus (`golden/toon/`).
6. **Count semantics (#165)** — `open_count` = status exactly `open`; partition invariant `not_closed == actionable + not_actionable`.

v0.25.0 also added five envelope fields Rust does not yet emit: `source_path`, `source_kind`, `source_authority`, `authority_hash`, `scope_hash`. Rust additionally emits `unblocks`, which v0.25.0 does not.

When porting any Go logic: read the Go source first (reference clone at `./beads_viewer/`, commit `18afafa`), copy constants EXACTLY, then write Rust. When in doubt, the Go code wins over this document.

Full design rationale: [COMPREHENSIVE_PLAN_FOR_FORT_BEADS_VIEWER.md](COMPREHENSIVE_PLAN_FOR_FORT_BEADS_VIEWER.md).

---

## Task Tracking: Beads (br)

This repo dogfoods Beads. Issues live in `.beads/issues.jsonl`; use `br` to manage:

```bash
br list            # all beads
br ready           # actionable work (no blockers)
br show <id>       # details
br update <id> --status in_progress
br close <id> --reason "done"
br dep cycles      # MUST be empty
br sync --flush-only   # after mutations; then git add .beads/ && commit
```

**NEVER run bare `bv`/`bvr`** — launches interactive TUI. Use `--robot-*` flags only (once the binary exists):

```bash
bvr --robot-triage     # prioritized recommendations
bvr --robot-next       # single top pick
```

Until our own binary works, use the Go reference build for triage if needed:
`cd beads_viewer && go run ./cmd/bv --robot-triage`

---

## Completion Workflow (MANDATORY per task)

1. Implement completely (no stubs, no TODO-left-behind).
2. `cargo fmt --all`
3. `cargo clippy --workspace --all-targets -- -D warnings`
4. `cargo test --workspace` (+ differential gates where applicable)
5. Commit with task-specific message referencing bead ID.
6. Push to `origin main`.
7. `br close <bead-id> --reason "..."` then `br sync --flush-only` and commit `.beads/` changes.

A task is NOT complete until formatted, tested, committed, AND pushed.

---

## Testing Philosophy

- **Differential-first**: Rust output must match frozen Go goldens byte-for-byte (modulo timestamps/timings).
- No mocks — real fixture data (synthetic JSONL graphs under `tests/fixtures/` once created).
- Property tests (proptest) for graph invariants; fuzz targets (cargo-fuzz) for the loader.
- Every algorithm constant (weights, tiers, timeouts, confidence ranges) comes from the audited Go source — never invent numbers.

---

## Upstream Sync Policy

- Parity phase: frozen on upstream `18afafa` (v0.25.0). Do NOT pull upstream changes. The previous target was `9ace029` (v0.20.0); goldens were rebaselined to v0.25.0 in commit `26c0b03`.
- **Regenerating goldens:** `scripts/capture_goldens.sh` only WARNs on a SHA mismatch and does not update `golden/METADATA.txt` by itself. Commands that fail (e.g. `--robot-history` on non-git fixtures) leave the existing golden untouched — verify `golden/METADATA.txt` `stale_pending_recapture` after any run.
- Post-v0.25.0-rust: monthly audits; cherry-pick compatible fixes; regenerate affected goldens.

---

## Note on Parallel Agent Work

Changes by other agents appearing mid-session are normal. NEVER stash/revert/overwrite them — treat as your own and build on top. Coordinate ownership through crate boundaries (each agent owns whole crates; contracts change only via the integration owner).
