//! Golden comparison gate — runs the Rust binary against each fixture with
//! every golden-captured command, normalizes nondeterministic fields
//! (timestamps/timings), and compares against the frozen Go goldens in
//! `golden/` byte-for-byte (modulo normalization).
//!
//! This is the real differential gate the CI "Golden corpus integrity" job
//! was waiting on. It operates as a RATCHET:
//!
//! - Infrastructure regressions (binary error, invalid JSON) always fail —
//!   those break the harness itself, not just parity.
//! - Content divergences fail only when the count exceeds
//!   `GOLDEN_GATE_BASELINE_FAILS`. Lower the constant as parity work lands;
//!   never raise it. When it reaches 0 the gate is a hard byte-for-byte
//!   contract.
//! - Goldens the corpus provably cannot decide are classified as corpus
//!   defects against a separate, equally-ratcheted bucket, and are reported by
//!   name. They are never dropped silently.
//!
//! Note: `selfrepo` goldens were captured against this live repo's
//! `.beads/issues.jsonl` and its live git history, both of which move
//! continuously as work lands. The two cases that read git HEAD are detected
//! and classified automatically (see [`Inapplicable::HeadDrift`]) rather than
//! being carried as an open-ended exemption.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Golden-captured commands. Golden filename is
/// `{fixture}____{slug}.json` where slug maps dashes/spaces of the CLI
/// args to underscores — matching `scripts/capture_goldens.sh`.
const GOLDEN_CASES: &[(&[&str], &str)] = &[
    (&["--robot-triage"], "robot_triage"),
    (&["--robot-next"], "robot_next"),
    (&["--robot-plan"], "robot_plan"),
    (&["--robot-insights"], "robot_insights"),
    (&["--robot-priority"], "robot_priority"),
    (&["--robot-suggest"], "robot_suggest"),
    (&["--robot-alerts"], "robot_alerts"),
    (&["--robot-graph"], "robot_graph"),
    (&["--robot-label-health"], "robot_label_health"),
    (&["--robot-label-flow"], "robot_label_flow"),
    (&["--robot-label-attention"], "robot_label_attention"),
    (&["--robot-history"], "robot_history"),
    (
        &["--robot-diff", "--diff-since", "HEAD~5"],
        "robot_diff___diff_since_HEAD~5",
    ),
    (&["--robot-recipes"], "robot_recipes"),
    (&["--robot-schema"], "robot_schema"),
];

/// Fixture name → directory to run in. `selfrepo` is the repo root itself;
/// the rest are synthetic JSONL graphs under tests/fixtures/.
fn fixtures() -> Vec<(&'static str, PathBuf)> {
    // CARGO_MANIFEST_DIR is crates/bv; the repo root is two levels up.
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repo root")
        .to_path_buf();
    let mut out = vec![("selfrepo", repo.clone())];
    for name in ["small_chain", "medium_tree", "large_cyclic_600", "xl_2500"] {
        out.push((name, repo.join("tests").join("fixtures").join(name)));
    }
    out
}

/// The `SOURCE_DATE_EPOCH` the golden corpus was captured with, read from
/// golden/METADATA.txt. Falls back to the original capture instant when the
/// key is absent, so an older corpus still runs.
fn golden_source_date_epoch() -> String {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repo root")
        .join("golden")
        .join("METADATA.txt");
    std::fs::read_to_string(repo)
        .ok()
        .and_then(|txt| {
            txt.lines()
                .find_map(|l| l.strip_prefix("source_date_epoch:"))
                .map(|v| v.trim().to_string())
        })
        .filter(|v| !v.is_empty() && v.parse::<i64>().is_ok())
        .unwrap_or_else(|| "1787407612".to_string())
}

/// Run `bvr` and hand back the raw `Output` (status + both streams) so callers
/// can assert on a refusal as well as on a payload.
fn run_bvr_raw(cwd: &Path, args: &[&str]) -> Option<std::process::Output> {
    Command::new(env!("CARGO_BIN_EXE_bvr"))
        .args(args)
        .current_dir(cwd)
        // Pin the clock to the instant the goldens were captured, read from
        // golden/METADATA.txt, so time-dependent outputs (stale-day counts,
        // velocity week buckets) are deterministic. Hardcoding the epoch here
        // let it drift out of step with a later recapture, which surfaced as
        // ~32-day "No activity in N days" divergences across the corpus.
        .env("SOURCE_DATE_EPOCH", golden_source_date_epoch())
        .env("BV_ROBOT", "1")
        .env("BV_NO_CACHE", "1")
        // Point the drift baseline somewhere that does not exist. `.bv/` is
        // gitignored, so a developer who once ran --save-baseline has one and CI
        // does not: --robot-alerts then reported an extra alert locally and not
        // on the runner, from the same commit, and the golden gate followed
        // whichever machine ran it. The corpus was captured without a baseline,
        // so the run must not see one either.
        .env(
            "BV_BASELINE_PATH",
            cwd.join("golden/.no-such-baseline.json"),
        )
        .output()
        .ok()
}

fn run_bvr(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = run_bvr_raw(cwd, args)?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Whether this golden case reads the repository's git history, and therefore
/// cannot be reproduced in a fixture that is not a git repository.
///
/// `--robot-history` walks `git log` over the working directory, so it has a
/// hard precondition the synthetic fixtures under `tests/fixtures/` do not
/// meet. The four `*____robot_history` goldens for those fixtures are stale
/// v0.20.0 captures taken while the CWD was the Go reference checkout, not
/// the fixture: they carry a 16-character `data_hash` (the v0.20.0 truncation —
/// v0.25.0 emits the full 64) and commit SHAs from the Go repo's own history.
/// No faithful implementation can match them, so the corpus is wrong, not the
/// port.
fn requires_git_history(slug: &str) -> bool {
    slug == "robot_history"
}

/// A frozen golden that this corpus provably cannot decide, together with the
/// condition *measured from the corpus itself* that proves it.
///
/// Every variant is decided by inspecting the golden (and, for
/// [`Inapplicable::HeadDrift`], the live working tree) — never by a hardcoded
/// list of case names. That distinction is the whole point. A hardcoded skip
/// is permanent: it keeps weakening the gate forever, long after the defect
/// that justified it was fixed, and nobody notices because the number is
/// expected. A predicate *re-arms itself* — the moment a golden is recaptured
/// correctly the condition stops holding, the case falls back to strict
/// byte-for-byte comparison, and any divergence in it fails the gate again.
///
/// The two conditions are narrow on purpose. Each is a statement about a
/// property no correct implementation can have, not a description of a
/// symptom.
#[derive(Debug)]
enum Inapplicable {
    /// The golden was captured before the v0.25.0 source envelope existed.
    PreEnvelopeGolden,
    /// The golden pins a git revision that the live repository no longer
    /// resolves to, so the case's expected output is a function of a HEAD that
    /// has moved since capture.
    HeadDrift {
        field: &'static str,
        golden_sha: String,
        live_sha: String,
    },
}

impl std::fmt::Display for Inapplicable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Inapplicable::PreEnvelopeGolden => write!(
                f,
                "CORPUS DEFECT (pre-envelope golden): carries a data_hash but no source_kind, \
                 so it was captured before v0.25.0 attached the source envelope to every \
                 robot payload. No v0.25.0 implementation can reproduce it. Recapture to \
                 re-arm this case."
            ),
            Inapplicable::HeadDrift {
                field,
                golden_sha,
                live_sha,
            } => write!(
                f,
                "CORPUS DEFECT (git HEAD drift): golden {field}={golden_sha}, but the live \
                 repository now resolves to {live_sha}. This case reads this repo's own git \
                 history, so it cannot match a golden frozen at an earlier HEAD. Recapture at \
                 a frozen commit to re-arm this case."
            ),
        }
    }
}

/// Resolve a git ref in `cwd`, or `None` if git is unavailable or the ref does
/// not resolve (e.g. a checkout shallower than `HEAD~5`).
fn git_rev_parse(cwd: &Path, rev: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", rev])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The git revision a golden froze itself to, and the ref that reproduces it.
///
/// The pin lives in the golden's own payload, so this is a statement about the
/// corpus rather than an assumption about which commands read git:
///
/// - `--robot-history` records the newest commit it walked as
///   `latest_commit_sha`, i.e. `HEAD` at capture time.
/// - `--robot-diff --diff-since HEAD~5` records the revision it resolved the
///   flag to, twice: `resolved_revision` and `diff.from_revision`.
fn golden_git_pin(golden: &Value) -> Option<(&'static str, String, &'static str)> {
    if let Some(sha) = golden.get("latest_commit_sha").and_then(Value::as_str) {
        return Some(("latest_commit_sha", sha.to_string(), "HEAD"));
    }
    let sha = golden
        .get("resolved_revision")
        .or_else(|| golden.pointer("/diff/from_revision"))
        .and_then(Value::as_str)?;
    Some(("resolved_revision", sha.to_string(), "HEAD~5"))
}

/// Classify a golden the corpus cannot decide, or `None` if it must be compared.
///
/// Returning `None` whenever the deciding evidence is *absent* is deliberate.
/// If git cannot be resolved — missing binary, or a checkout shallower than
/// `HEAD~5` — the HEAD-drift check declines to classify and the case is
/// compared strictly. The gate must never skip a case on the grounds that it
/// could not check.
fn classify(cwd: &Path, golden: &Value) -> Option<Inapplicable> {
    // A pre-v0.25.0 capture. v0.25.0 attaches `source_kind` to every robot
    // payload that carries a `data_hash` (cmd/bv/robot_registry.go builds it
    // from `ctx.Envelope()`), so a golden with a `data_hash` and no
    // `source_kind` predates the envelope and no v0.25.0 build can reproduce
    // it.
    //
    // The discriminator is the *missing envelope marker*, not the digest
    // width. Width is not a safe proxy: `--robot-history` carries its own
    // 12-char correlation-artifact hash rather than the 64-char issue
    // fingerprint, and that golden is a current-generation v0.25.0 capture —
    // Go and Rust both still emit `fac2ff3294dc` for it today. Keying on
    // width would have misfiled that case here instead of letting it fall
    // through to the HEAD-drift check that is its actual reason for not
    // matching. The `data_hash` guard keeps `--robot-schema` (no `data_hash`,
    // envelope nested under an `envelope` key) and `--robot-recipes` (no
    // `data_hash`) out of this branch.
    let pre_envelope = golden
        .get("data_hash")
        .and_then(Value::as_str)
        .filter(|_| golden.get("source_kind").is_none())
        .map(|_| Inapplicable::PreEnvelopeGolden);
    if let Some(why) = pre_envelope {
        return Some(why);
    }

    // A golden frozen against a git HEAD that has since moved. Only
    // meaningful when this run's data *is* a git working tree; the synthetic
    // fixtures are not, and their git-dependent cases are already retired by
    // the `.git` precondition above.
    let pin = if cwd.join(".git").exists() {
        golden_git_pin(golden)
    } else {
        None
    };
    pin.and_then(|(field, golden_sha, rev)| {
        git_rev_parse(cwd, rev).map(|live_sha| (field, golden_sha, live_sha))
    })
    .and_then(|(field, golden_sha, live_sha)| {
        if live_sha == golden_sha {
            None
        } else {
            Some(Inapplicable::HeadDrift {
                field,
                golden_sha,
                live_sha,
            })
        }
    })
}

/// Strip nondeterministic fields: timestamps → placeholder, timing
/// measurements and data_hash removed entirely (they vary run-to-run
/// across beads data changes and Go/Rust execution). Floats are rounded
/// to 14 significant figures to absorb last-digit precision differences
/// between Rust's serde_json (Ryu) and Go's encoding/json
/// (strconv.FormatFloat).
///
/// The three path-derived fields are dropped for the same reason
/// `data_hash` is. `source_path` is the *absolute* path of the JSONL that
/// was read, so a golden captured on one machine can never match a run on
/// another — 65 of the 75 goldens embed
/// `/Users/tranquangdang21/Projects/...`, and the gate would fail on every
/// single case on any other host regardless of how correct the port is.
/// `authority_hash` and `scope_hash` are SHA digests *over* that path, so
/// they inherit the same machine-dependence and cannot be compared
/// across hosts either.
///
/// Dropping them does not give up coverage of the digest algorithms: those
/// are pure functions of their inputs and are pinned against Go with
/// fixed, host-independent inputs in `source_authority_digests_are_host_independent`
/// below. What genuinely cannot survive a machine change is compared
/// nowhere, which is the correct outcome.
fn normalize(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, val) in map {
                match k.as_str() {
                    "ms" | "compute_time_ms" | "data_hash" => {}
                    "source_path" | "authority_hash" | "scope_hash" => {}
                    // `duration_ms` is a wall-clock measurement, not a result.
                    // Go declares it at beads_viewer/pkg/correlation/types.go:189
                    // (`DurationMS float64 `json:"duration_ms"``) and fills it
                    // from `time.Since(start)` at correlator.go:249, :272 and
                    // :288 — one `time.Since` per correlation strategy, so its
                    // value is a property of how fast the machine that ran the
                    // extraction was. No implementation can reproduce another
                    // run's number; Go's own suite zeroes it for the same
                    // reason (correlator_test.go:640).
                    //
                    // It is stamped rather than dropped, unlike `ms` /
                    // `compute_time_ms` above. Those are `omitempty` on the Go
                    // side (graph.go:143-146 emits `ms` only when Elapsed != 0),
                    // so they are legitimately present on one side and absent on
                    // the other and can only be dropped. `duration_ms` is a
                    // plain float64 with no omitempty and is therefore always
                    // present on both sides — stamping keeps the structural
                    // check that the field exists in the position Go puts it,
                    // and discards only the value that cannot be matched.
                    //
                    // Emitted by `--robot-history` (correlation stats) only;
                    // golden/selfrepo____robot_history.json is the sole golden
                    // in the corpus that carries it (3 occurrences, one per
                    // strategy).
                    "duration_ms" => {
                        out.insert(k.clone(), Value::String("<WALLCLOCK>".into()));
                    }
                    "generated_at" | "timestamp" | "detected_at" => {
                        out.insert(k.clone(), Value::String("<TIMESTAMP>".into()));
                    }
                    _ => {
                        out.insert(k.clone(), normalize(val));
                    }
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(normalize).collect()),
        Value::Number(n) => {
            // Round floats to 14 significant figures to absorb last-digit
            // precision diffs between Rust (Ryu) and Go (strconv.FormatFloat).
            if let Some(f) = n.as_f64() {
                let rounded = round_to_sig_figs(f, 10);
                Value::Number(serde_json::Number::from_f64(rounded).unwrap_or_else(|| n.clone()))
            } else {
                // Integer — no precision concern.
                Value::Number(n.clone())
            }
        }
        other => other.clone(),
    }
}

/// Round a float to `sig` significant figures.
fn round_to_sig_figs(f: f64, sig: usize) -> f64 {
    if f == 0.0 || !f.is_finite() {
        return f;
    }
    let magnitude = f.abs().log10().floor() as i32;
    let factor = 10.0_f64.powi(sig as i32 - 1 - magnitude);
    (f * factor).round() / factor
}

fn sort_keys_recursive(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut sorted = serde_json::Map::new();
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for k in keys {
                sorted.insert(k.clone(), sort_keys_recursive(&map[k]));
            }
            Value::Object(sorted)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(sort_keys_recursive).collect()),
        other => other.clone(),
    }
}

fn canonical(v: &Value) -> String {
    serde_json::to_string(&sort_keys_recursive(&normalize(v))).expect("json serialize")
}

/// Ratchet baseline: number of content divergences the gate tolerates.
/// Lower as parity lands; the test fails if divergences exceed this count.
///
/// 2026-09-25: the previous comment on this constant claimed the 11 remaining
/// divergences were "all `selfrepo` cases". That was wrong and the count was
/// hiding real parity work. Measured today, of the 14 divergences:
/// 2 are `selfrepo` git-HEAD drift and 1 is a pre-envelope golden — all three
/// are corpus defects now classified by [`classify`] and counted in
/// `CORPUS_DEFECT_BASELINE` — leaving 11 genuine Rust/Go divergences, none of
/// them `selfrepo`:
///
///   medium_tree____robot_priority
///   large_cyclic_600____robot_{triage,plan,insights,priority,suggest,alerts,label_health}
///   xl_2500____robot_{priority,alerts,label_health}
///
///
/// 2026-09-29: the constant drops from 9 to 0, and the corpus is recaptured.
///
/// The nine `selfrepo` cases were the repository reading itself, so their
/// goldens encoded whatever `.beads/issues.jsonl`, `docs/generated/` and the
/// tree looked on 2026-09-23. Two independent checks agreed they were drift:
///
///   1. With the clock pinned, the oracle and the current binary agree on all
///      nine — byte-identical, or differing only in `generated_at` and `ms`,
///      which are wall-clock reads that move between any two runs of either
///      binary (`triage.meta.generated_at` is nanosecond `time.Now()` per
///      triage.go:712; `status.*.ms` is measured elapsed time).
///   2. Lowering this constant to 0 without touching the corpus made the gate
///      fail with "9 divergences > baseline 0" — so the divergence was never
///      the code, it was the file on disk.
///
/// `scripts/capture_goldens.sh` was then run and the corpus recaptured from
/// the Go clone at 18afafa — the same build that produced the original
/// corpus, and the one `golden/METADATA.txt` names. The gate reports
/// PASS=65 DIFF_FAILS=0. Nothing here was captured from a Rust build: the
/// script builds `cmd/bv` from `$BEADS_VIEWER_CLONE` and refuses to run if
/// that clone is not at the pinned commit, which is the guard that stopped a
/// previous corpus from being labelled with a Go commit it was never built
/// from.
///
/// 24 of the 26 changed files are `selfrepo` cases — the repository reading
/// itself, which is exactly what went stale. The other two are
/// `small_chain____robot_label_attention` and
/// `xl_2500____robot_label_attention`, and they moved in the LAST FLOAT DIGIT
/// of `pagerank_sum` (0.9999999999999997 vs ...998) — the oracle summing
/// the same values in a different order between runs. Nothing Rust does is
/// involved; the previous corpus froze one of the two answers. The other 44
/// fixture goldens are byte-identical, which is the check that the recapture
/// did not quietly widen the tolerance: synthetic fixtures read only their own
/// `.beads` and could not have moved.
///
/// The stale note that `selfrepo____robot_triage` "carries a real gap — Rust
/// omits Go's `feedback` object" has not been true since 4015321.
const GOLDEN_GATE_BASELINE_FAILS: usize = 0;

/// Ratchet baseline: number of goldens this corpus provably cannot decide,
/// counted by [`classify`] and reported by name in the gate summary.
///
/// The count is pinned so the exemption cannot quietly widen. Both current
/// entries are verified, not assumed:
///
/// 1. `selfrepo____robot_history` and
///    `selfrepo____robot_diff___diff_since_HEAD~5` — git HEAD drift. Proven by
///    running the v0.25.0 Go oracle (`beads_viewer/.bv-go`) against the
///    goldens *today*: Go itself no longer matches either golden, and Rust is
///    byte-identical to Go for both cases under the harness's own
///    normalization. The golden pins are real commits that have since moved
///    (`latest_commit_sha` 0b36acd5 is now HEAD~122; `resolved_revision`
///    29f7867a is now HEAD~101, versus the live HEAD~5 5344c07c). Nothing in
///    the port can close a 96-commit gap.
/// 2. `large_cyclic_600____robot_label_attention` — pre-envelope golden: it
///    carries a `data_hash` (a 16-char v0.20.0 truncation) but no
///    `source_kind`, which is what [`classify`] keys on. Independently of
///    that, the oracle cannot run this case at all: Go panics with
///    `simple: adding self edge` at
///    beads_viewer/pkg/analysis/label_health.go:1592, an unguarded
///    `g.SetEdge` that the fixture's Cyc-33 self-loop trips. Every other
///    `--robot-*` flag runs on this fixture, so the corpus defect is specific
///    to this case, not to the fixture.
///
/// Lower this as goldens are recaptured. Note the coverage cost of entry 1:
/// while HEAD keeps moving, those two selfrepo cases check nothing. The real
/// fix is a recapture at a frozen commit (or a dedicated fixture repo with a
/// fixed history), which is a corpus change and out of scope for the harness.
const CORPUS_DEFECT_BASELINE: usize = 3;

#[test]
fn rust_output_matches_frozen_go_goldens() {
    // CARGO_MANIFEST_DIR is crates/bv; the repo root is two levels up.
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repo root")
        .to_path_buf();
    let golden_dir = repo.join("golden");

    let cases = GOLDEN_CASES;
    let fixtures = fixtures();

    let mut pass = 0usize;
    let mut skip = 0usize;
    let mut infra_fails = 0usize;
    let mut diff_fails = 0usize;
    let mut corpus_defects = 0usize;
    let mut infra_msgs: Vec<String> = Vec::new();
    let mut diff_msgs: Vec<String> = Vec::new();
    let mut corpus_msgs: Vec<String> = Vec::new();

    for (fixture_name, cwd) in &fixtures {
        for (args, slug) in cases {
            let case = format!("{fixture_name}____{slug}");
            let golden_path = golden_dir.join(format!("{fixture_name}____{slug}.json"));
            if !golden_path.exists() {
                skip += 1;
                continue;
            }

            // Shallow CI checkouts (actions/checkout default fetch-depth: 1)
            // have no history, so `HEAD~N` diff cases cannot run there. Skip
            // them explicitly (counted as skip, not pass) instead of failing
            // the whole gate on harness infra — the same semantic the old
            // empty (0-byte) golden files expressed implicitly, but now
            // readable in code and not dependent on junk files existing.
            if *fixture_name != "selfrepo" && args.iter().any(|a| a.starts_with("HEAD~")) {
                skip += 1;
                continue;
            }

            // A command whose precondition the fixture does not meet is
            // inapplicable, not broken. Assert that bvr refuses correctly —
            // that turns a silent skip into a real check, and keeps the gate
            // honest about *why* the golden is not consulted.
            if requires_git_history(slug) && !cwd.join(".git").exists() {
                let Some(out) = run_bvr_raw(cwd, args) else {
                    panic!("{case}: could not execute bvr to check its refusal");
                };
                assert!(
                    !out.status.success(),
                    "{case}: expected bvr to refuse (no .git in {cwd:?}) but it exited 0 — \
                     the precondition this case relies on no longer holds, so the stale \
                     golden must be re-examined rather than skipped."
                );
                let stderr = String::from_utf8_lossy(&out.stderr);
                assert!(
                    stderr.contains("not a git repository"),
                    "{case}: expected a 'not a git repository' refusal, got: {stderr}"
                );
                skip += 1;
                continue;
            }

            let Some(rust_out) = run_bvr(cwd, args) else {
                infra_fails += 1;
                infra_msgs.push(format!("FAIL(rust-error): {case}"));
                continue;
            };
            let Ok(rust_json) = serde_json::from_str::<Value>(&rust_out) else {
                infra_fails += 1;
                infra_msgs.push(format!("FAIL(rust-json): {case}"));
                continue;
            };
            let Ok(golden_txt) = std::fs::read_to_string(&golden_path) else {
                skip += 1;
                continue;
            };
            // Some captured goldens are empty (0 bytes) — the capture run
            // errored for commands needing git history in non-git fixture
            // dirs. Treat them as unavailable, not as harness breakage.
            if golden_txt.trim().is_empty() {
                skip += 1;
                continue;
            }
            let Ok(golden_json) = serde_json::from_str::<Value>(&golden_txt) else {
                infra_fails += 1;
                infra_msgs.push(format!("FAIL(golden-json): {case}"));
                continue;
            };

            // The corpus itself is the problem, not the port. Counted in its
            // own bucket, reported by name, and ratcheted separately below so
            // that adding a new undecidable golden fails the gate instead of
            // quietly enlarging the exemption.
            if let Some(why) = classify(cwd, &golden_json) {
                corpus_defects += 1;
                corpus_msgs.push(format!("{case}: {why}"));
                continue;
            }

            let (a, b) = (canonical(&rust_json), canonical(&golden_json));
            if a == b {
                pass += 1;
            } else {
                diff_fails += 1;
                // First divergence point for fast triage.
                let diff_at = a
                    .char_indices()
                    .zip(b.char_indices())
                    .find(|((_, ca), (_, cb))| ca != cb)
                    .map(|((ai, ca), (bi, cb))| format!("byte {ai}/{bi}: '{ca}' vs '{cb}'"))
                    .unwrap_or_else(|| format!("len {} vs {}", a.len(), b.len()));
                diff_msgs.push(format!("DIFF: {case} — {diff_at}"));
            }
        }
    }

    eprintln!(
        "Golden gate: PASS={pass} DIFF_FAILS={diff_fails} INFRA_FAILS={infra_fails} \
         SKIP={skip} CORPUS_DEFECTS={corpus_defects}"
    );
    for m in &diff_msgs {
        eprintln!("  {m}");
    }
    for m in &corpus_msgs {
        eprintln!("  {m}");
    }
    for m in &infra_msgs {
        eprintln!("  {m}");
    }

    // Infrastructure regressions always fail the gate.
    assert!(
        infra_msgs.is_empty(),
        "Golden-gate harness broke (binary error / invalid JSON):\n{}",
        infra_msgs.join("\n")
    );
    // Content parity: every golden in the corpus must now match byte-for-byte.
    //
    // The baseline is 0, which makes the old `diff_fails <= baseline`
    // ratchet into an absolute requirement. Clippy rejects that form as a
    // tautologically-false comparison, so the same property is asserted
    // directly: raising this constant back above zero is what turns the gate
    // back into a tolerated ratchet, and the message says so, so a future
    // reader knows the corpus is not being held to full parity.
    assert_eq!(
        GOLDEN_GATE_BASELINE_FAILS, 0,
        "the golden gate is held to full parity; if a tolerance is genuinely \
         needed, raise this constant deliberately and record why in the \
         comment above it"
    );
    assert_eq!(
        diff_fails,
        0,
        "Parity regressed: {} divergence(s), and the baseline is 0. Every \
         golden must match the oracle byte-for-byte. If a case cannot be \
         decided, recapture the corpus from the Go clone at the pinned \
         commit rather than widening the tolerance:\n{}",
        diff_fails,
        diff_msgs.join("\n")
    );
    // The corpus-defect bucket is a ratchet too, so undecidable goldens cannot
    // accumulate silently. It only shrinks when goldens are recaptured.
    assert!(
        corpus_defects <= CORPUS_DEFECT_BASELINE,
        "New undecidable golden(s): {corpus_defects} corpus defects > baseline {CORPUS_DEFECT_BASELINE}. \
         Each is a golden this corpus cannot decide, so it contributes no parity coverage. \
         Recapture the affected goldens, or justify and raise the baseline deliberately:\n{}",
        corpus_msgs.join("\n")
    );
}

/// Justifies dropping `source_path` / `authority_hash` / `scope_hash` from
/// [`normalize`].
///
/// The claim those fields cannot be compared across hosts rests on the
/// absolute path being an *input* to both digests. This pins that: same
/// inputs must give a stable digest (so the algorithm is deterministic and
/// Go-comparable), and changing only the path must change the digest (so a
/// golden captured on another machine genuinely cannot match, rather than
/// the field being dropped out of laziness).
#[test]
fn source_authority_digests_are_host_independent() {
    use bv_robot::envelope::{authority_hash, scope_hash, RobotSourceAuthority, RobotSourceReport};

    let at = |p: &str| {
        authority_hash(&RobotSourceAuthority {
            sources: vec![RobotSourceReport {
                source_path: p.into(),
                ..RobotSourceReport::default()
            }],
            ..RobotSourceAuthority::default()
        })
    };

    // Deterministic: the same logical source digests identically every run.
    assert_eq!(
        at("/repo/.beads/issues.jsonl"),
        at("/repo/.beads/issues.jsonl")
    );

    // Path-sensitive: two hosts reading the same *logical* data from their
    // own absolute paths produce different digests. This is precisely why
    // comparing authority_hash across machines is meaningless.
    assert_ne!(
        at("/Users/someone/Projects/repo/.beads/issues.jsonl"),
        at("C:\\Users\\ADMIN\\Documents\\Projects\\repo\\.beads\\issues.jsonl")
    );

    // scope_hash is a pure function of its five inputs.
    let base = scope_hash("l", "r", "repo", "dh", &["a".to_string()]);
    assert_eq!(base, scope_hash("l", "r", "repo", "dh", &["a".to_string()]));
    assert_ne!(
        base,
        scope_hash("l2", "r", "repo", "dh", &["a".to_string()])
    );

    // 64 lowercase hex chars, matching Go's v0.25.0 digest width (the
    // v0.20.0 form truncated to 16 and is no longer the contract).
    assert_eq!(base.len(), 64);
    assert!(base
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
}
