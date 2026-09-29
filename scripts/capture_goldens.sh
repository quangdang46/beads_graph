#!/usr/bin/env bash
# Capture frozen Go-oracle goldens from the reference clone.
# Usage: scripts/capture_goldens.sh [output_dir=golden]
#
# The captured corpus IS the project's parity oracle, so a run that silently
# captures from the wrong build is worse than no run at all: it re-bakes false
# provenance and the gate keeps passing or failing for reasons that have
# nothing to do with the port. Every guard below exists because that happened.
set -euo pipefail

OUT="${1:-golden}"
# The Go clone is a SIBLING of this repo, not a child. Resolve it from the
# script's own location so the script works from any working directory —
# `cd`-relative resolution silently tripped the existence guard when run from
# the repo root.
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
UPSTREAM="${BEADS_VIEWER_CLONE:-$REPO/../beads_viewer}"
# Upstream is frozen for the parity phase. v0.25.0.
COMMIT="18afafaefbcfd7fbb2ed4f55b49e7141da6513d2"

# Pin the clock for capture. Go honors SOURCE_DATE_EPOCH (main.go:1165), so
# time-dependent output (staleness day counts, velocity week buckets) is
# reproducible. Without this, goldens are stamped at wall-clock time and
# drift out of step with the epoch golden_comparison pins, which showed up as
# ~32-day "No activity in N days" divergences across the whole corpus.
# Override with: SOURCE_DATE_EPOCH=<unix-seconds> scripts/capture_goldens.sh
export SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-1787407612}"

[ -d "$UPSTREAM/.git" ] || {
  echo "ERROR: Go reference clone not found at $UPSTREAM" >&2
  echo "       Set BEADS_VIEWER_CLONE=/path/to/beads_viewer" >&2
  exit 1
}

current=$(git -C "$UPSTREAM" rev-parse HEAD)
if [ "$current" != "$COMMIT" ]; then
  # Hard failure, not a warning. The previous corpus in git/ was labelled with
  # a Go commit it was never built from (commit 4513446 recaptured all 75 files
  # from the *Rust* binary; 26c0b03 then claimed a v0.25.0 rebaseline without
  # recapturing), and a soft warning is what let that happen twice.
  echo "ERROR: Go clone is at $current, expected $COMMIT." >&2
  echo "       The parity target is frozen; check out $COMMIT or point" >&2
  echo "       BEADS_VIEWER_CLONE at a clone that is." >&2
  exit 1
fi

echo "Building Go bv from $current..."
( cd "$UPSTREAM" && go build -o "$REPO/scripts/.bv-go" ./cmd/bv )

BV="$REPO/scripts/.bv-go"
cd "$REPO"

# Fixture classes: real repo + synthetic fixtures
declare -a FIXTURES=(
    "."
    "tests/fixtures/small_chain"
    "tests/fixtures/medium_tree"
    "tests/fixtures/large_cyclic_600"
    "tests/fixtures/xl_2500"
)

declare -a COMMANDS=(
    "--robot-triage"
    "--robot-next"
    "--robot-plan"
    "--robot-insights"
    "--robot-priority"
    "--robot-suggest"
    "--robot-alerts"
    "--robot-graph"
    "--robot-label-health"
    "--robot-label-flow"
    "--robot-label-attention"
    "--robot-history"
    "--robot-diff --diff-since HEAD~5"
    "--robot-recipes"
    "--robot-schema"
)

mkdir -p "$OUT"
captured=0

# A `.bv/baseline.json` under a fixture root poisons the corpus, and it is the
# one input a capture cannot control. Go's `--robot-alerts` and `--robot-triage`
# read the drift baseline relative to the working directory, and `.bv/` is
# gitignored — so a machine that has ever run `--save-baseline` carries a
# baseline that a fresh clone (and CI) does not. Capturing the `selfrepo` class
# with one present froze a local reading into the oracle: golden/
# selfrepo____robot_alerts.json carried
#   "Actionable issues decreased by 2 (100.0%)", baseline_value 2
# which no clean machine can reproduce, because the 2 came from the capture
# machine's own workspace state. The Rust port matched it here only because
# this machine had the same file; CI, without it, read an empty alert list and
# the gate failed for a reason that had nothing to do with the port.
#
# Hard failure, same reasoning as the commit guard above: silently capturing
# local state is worse than not capturing. Move the baseline aside (or commit
# a decision to baseline-dependent goldens) and re-run.
if [ -f "$REPO/.bv/baseline.json" ]; then
  echo "ERROR: $REPO/.bv/baseline.json exists." >&2
  echo "       --robot-alerts/--robot-triage would read it and freeze this" >&2
  echo "       machine's drift state into the corpus. Move it aside and" >&2
  echo "       re-run, e.g.  mv .bv/baseline.json /tmp/  (do not delete it)." >&2
  exit 1
fi

for fixture in "${FIXTURES[@]}"; do
    name=$(basename "$fixture")
    [ "$name" = "." ] && name="selfrepo"
    for cmd in "${COMMANDS[@]}"; do
        slug=$(echo "$cmd" | tr ' -' '__')
        f="$OUT/${name}__${slug}.json"
        # Capture to a temp file first: a bare `> "$f"` redirect truncates the
        # golden BEFORE the command runs, so a command that fails (e.g.
        # --robot-history outside a git repo) silently leaves a 0-byte golden
        # that looks like a legitimate capture. Only replace the golden on
        # success, so a failed command leaves the previous corpus intact.
        tmp="$f.tmp"
        rm -f "$tmp"
        # `set -e` is disabled around the capture so a command that legitimately
        # fails (e.g. --robot-history outside a git repo) is reported as a SKIP
        # rather than aborting the whole run before the status is inspected.
        if [ "$fixture" != "." ]; then
            set +e
            (cd "$fixture" && BV_NO_CACHE=1 BV_TEST_MODE=1 "$BV" $cmd > "$REPO/$tmp" 2>/dev/null)
            cmd_status=$?
            set -e
        else
            set +e
            (BV_NO_CACHE=1 BV_TEST_MODE=1 "$BV" $cmd > "$tmp" 2>/dev/null)
            cmd_status=$?
            set -e
        fi
        if [ "$cmd_status" -ne 0 ] || [ ! -s "$tmp" ]; then
            echo "SKIP (cmd failed, golden left unchanged): $name $cmd"
            rm -f "$tmp"
        else
            mv "$tmp" "$f"
            captured=$((captured + 1))
        fi
    done
done

# TOON corpus for the commands that support it
mkdir -p "$OUT/toon"
for cmd in "--robot-triage" "--robot-next" "--robot-plan" "--robot-insights" "--robot-graph" "--robot-history"; do
    slug=$(echo "$cmd" | tr ' -' '__')
    (BV_NO_CACHE=1 BV_TEST_MODE=1 "$BV" $cmd --format toon > "$OUT/toon/selfrepo${slug}.toon" 2>/dev/null) || echo "SKIP toon: $cmd"
done

rm -f "$BV"

# Write provenance only if something was actually captured, and only after the
# oracle's own commit has been re-checked. The previous script stamped
# METADATA unconditionally, so a run that captured nothing — or captured from
# the wrong build — still produced a file asserting a Go commit, which is how
# a corpus built from the Rust binary came to be labelled `18afafa`.
if [ "$captured" -eq 0 ]; then
    echo "ERROR: nothing was captured; refusing to write METADATA.txt" >&2
    exit 1
fi

built_from=$(git -C "$UPSTREAM" rev-parse HEAD)
if [ "$built_from" != "$COMMIT" ]; then
    echo "ERROR: Go clone moved to $built_from during the run; refusing to write METADATA.txt" >&2
    exit 1
fi

# `source_date_epoch` is the instant the capture was pinned to; golden_comparison
# reads it so the test clock can never drift from the corpus.
{
    echo "captured_at: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "producer: go-bv-oracle"
    echo "go_commit: $built_from"
    echo "expected_commit: $COMMIT"
    echo "source_date_epoch: $SOURCE_DATE_EPOCH"
    echo "captured_files: $captured"
} > "$OUT/METADATA.txt"

echo "Captured $captured files from Go $built_from into $OUT/"
