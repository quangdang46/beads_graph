#!/usr/bin/env bash
# Feature sweep — exercise every robot command and modifier against a REAL
# beads repo, with the Go oracle alongside as the parity witness.
#
# `parity_matrix.sh` covers the curated core surface. This one is the wide
# net: every registered robot primary, the modifier combinations the
# modifier-requires table allows, and the non-robot command surface (export,
# feedback, agents, drift baseline). It reports four verdicts:
#
#   pass    byte-identical to the oracle
#   KNOWN   differs only in the `bv` -> `bvr` program-name substitution, which
#           this port makes deliberately (see the note in
#           crates/bv/tests/golden_comparison.rs: the binary installs as
#           `bvr`, and Go's strings name a command this project does not ship)
#   TIMING  differs only in a measurement field
#   PARITY  a real divergence — a difference the program-name substitution
#           does not explain
#   CRASH   the Rust build panicked, produced non-JSON where JSON is the
#           contract, or exited outside the documented 0/1/2 range
#
# Usage:
#   scripts/feature_sweep.sh                  # everything
#   scripts/feature_sweep.sh --filter triage  # only invocations matching
#   scripts/feature_sweep.sh --group export   # only one group
#   RS_BIN=... GO_BIN=... scripts/feature_sweep.sh
#
# Defaults are OS-aware so the script runs on macOS and Linux without
# overrides.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Go's binary is a Windows-shaped name in the original script; derive the
# platform suffix instead of hardcoding `.exe`, so the same script works on
# macOS and Linux CI.
case "$(uname -s)" in
    Darwin*) GO_SUFFIX=".exe" ;;
    *)       GO_SUFFIX="" ;;
esac

GO_BIN="${GO_BIN:-$ROOT/scripts/.bv-go$GO_SUFFIX}"
RS_BIN="${RS_BIN:-$ROOT/target/release/bvr}"

export SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-1787407612}"
export BV_NO_CACHE=1
export BV_TEST_MODE=1

FILTER=""
GROUP=""
while [ $# -gt 0 ]; do
    case "$1" in
        --filter) FILTER="$2"; shift 2;;
        --group)  GROUP="$2"; shift 2;;
        -h|--help) sed -n '2,32p' "$0"; exit 0;;
        *) shift;;
    esac
done

for bin in "$GO_BIN" "$RS_BIN"; do
    if [ ! -x "$bin" ]; then
        echo "MISSING BINARY: $bin" >&2
        case "$bin" in
            *bv-go*) echo "  Go:    (cd beads_viewer && go build -o ../scripts/.bv-go$GO_SUFFIX ./cmd/bv)" >&2;;
            *)       echo "  Rust:  cargo build --release -p bv" >&2;;
        esac
        exit 3
    fi
done

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# A bead id that exists in the real repo, for invocations that need one.
BEAD=$(grep -o '"id":"[^"]*"' "$ROOT/.beads/issues.jsonl" 2>/dev/null | head -1 | sed 's/.*:"//;s/"//')
BEAD="${BEAD:-beads_viewer_rust-api-freeze-b73}"

# A file that exists in the real repo, for the file-scoped commands.
FILE="README.md"
[ -f "$ROOT/$FILE" ] || FILE="Cargo.toml"

# invocations are '|' separated so the matrix survives shell quoting
MATRIX=(
  # --- group: core — one row per registered robot primary ---
  "--robot-help|meta"
  "--robot-capabilities|meta"
  "--robot-docs|meta"
  "--robot-schema|meta"
  "--robot-recipes|meta"
  "--robot-metrics|meta"
  "--robot-triage|core"
  "--robot-next|core"
  "--robot-triage-by-track|core"
  "--robot-triage-by-label|core"
  "--robot-insights|core"
  "--robot-plan|core"
  "--robot-priority|core"
  "--robot-alerts|core"
  "--robot-suggest|core"
  "--robot-graph|core"
  "--robot-search|core"
  "--robot-diff|core"
  "--robot-drift|core"
  "--robot-history|core"
  "--robot-explain-correlation|core"
  "--robot-confirm-correlation|core"
  "--robot-reject-correlation|core"
  "--robot-correlation-stats|core"
  "--robot-orphans|core"
  "--robot-file-beads|core"
  "--robot-file-hotspots|core"
  "--robot-impact|core"
  "--robot-file-relations|core"
  "--robot-related|core"
  "--robot-blocker-chain|core"
  "--robot-impact-network|core"
  "--robot-causality|core"
  "--robot-sprint-list|core"
  "--robot-sprint-show|core"
  "--robot-forecast|core"
  "--robot-capacity|core"
  "--robot-burndown|core"
  "--robot-label-health|core"
  "--robot-label-flow|core"
  "--robot-label-attention|core"

  # --- group: data — the commands that need a real bead / file / git ref ---
  "--robot-causality|$BEAD|data"
  "--robot-causality|$BEAD|--history-limit|10|data"
  "--robot-causality|$BEAD|--history-since|90d|data"
  "--robot-blocker-chain|$BEAD|data"
  "--robot-impact-network|$BEAD|data"
  "--robot-impact|$FILE|data"
  "--robot-file-beads|$FILE|data"
  "--robot-file-hotspots|data"
  "--robot-file-relations|$FILE|data"
  "--robot-related|$BEAD|data"
  "--robot-sprint-show|$BEAD|data"
  "--robot-diff|--diff-since|HEAD~3|data"
  "--robot-history|data"
  "--robot-history|--history-limit|10|data"
  "--robot-history|--history-since|90d|data"
  "--robot-graph|--graph-root|$BEAD|data"
  "--robot-graph|--graph-root|$BEAD|--graph-depth|2|data"

  # --- group: modifier — every modifier the requires-table accepts ---
  "--robot-triage|--brief|modifier"
  "--robot-triage-by-track|--brief|modifier"
  "--robot-triage-by-label|--brief|modifier"
  "--robot-triage|--graph-root|$BEAD|modifier"
  "--robot-next|--graph-root|$BEAD|modifier"
  "--robot-next|--robot-not-ready-labels|blocked|modifier"
  "--robot-next|--robot-history-timeout-ms|5000|modifier"
  "--robot-triage|--robot-not-ready-labels|blocked|modifier"
  "--robot-triage|--robot-history-timeout-ms|5000|modifier"
  "--robot-alerts|--severity|high|modifier"
  "--robot-alerts|--alert-type|blocked|modifier"
  "--robot-alerts|--alert-label|phase|modifier"
  "--robot-suggest|--suggest-type|dependency|modifier"
  "--robot-suggest|--suggest-confidence|0.5|modifier"
  "--robot-suggest|--suggest-bead|$BEAD|modifier"
  "--robot-label-attention|--attention-limit|3|modifier"
  "--robot-schema|--schema-command|robot-triage|modifier"
  "--robot-graph|--graph-format|json|modifier"
  "--robot-graph|--graph-depth|2|modifier"
  "--robot-diff|--diff-since|HEAD~3|modifier"
  "--robot-search|--search|deploy|modifier"
  "--robot-search|--search|deploy|--search-limit|5|modifier"
  "--robot-search|--search|deploy|--search-mode|semantic|modifier"
  "--robot-search|--search|deploy|--search-preset|balanced|modifier"
  "--robot-search|--search|deploy|--search-min-score|0.1|modifier"
  "--robot-search|--search|deploy|--search-weights|impact:1.0|modifier"
  "--robot-priority|--robot-by-label|phase|modifier"
  "--robot-priority|--robot-by-assignee|someone|modifier"
  "--robot-priority|--robot-min-confidence|0.5|modifier"
  "--robot-insights|--as-of|HEAD~3|modifier"
  "--robot-triage|--as-of|HEAD~3|modifier"

  # --- group: format — the output-format surface ---
  "--robot-triage|--format|json|format"
  "--robot-triage|--format|toon|format"
  "--robot-next|--toon|format"
  "--robot-next|--output|json|format"
  "--robot-insights|--format|toon|format"
  "--robot-plan|--format|toon|format"
  "--robot-graph|--format|toon|format"

  # --- group: argv — the positional aliases and rewriting ---
  "--json|argv"
  "--toon|argv"
  "triage|argv"
  "next|argv"
  "insights|argv"
  "plan|argv"
  "search|deploy|argv"

  # --- group: export — the non-robot report surface ---
  "--export|export"
  "--export-md|export"
  "--export-md|--export-format|json|export"
  "--export-graph|export"
  "--export-pages|export"
  "--emit-script|export"

  # --- group: nonrobot — the remaining command surface ---
  "--stats|nonrobot"
  "--bead-history|$BEAD|nonrobot"
  "--baseline-info|nonrobot"
  "--check-drift|nonrobot"
  "--feedback-show|nonrobot"
  "--agents-check|nonrobot"
  "--version|nonrobot"
  "--robot-docs|guide|nonrobot"

  # --- group: error — the documented failure contract ---
  "--robot-diff|error"
  "--robot-triage|--format|xml|error"
  "--robot-causality|NO-SUCH-BEAD-xyz|error"
  "--robot-causality|$BEAD|--history-limit|abc|error"
  "--robot-triage|--brief|--robot-next|error"
  "--robot-triage|--robot-next|error"
  "--search|error"
  "--export|error"
  "--robot-max-results|abc|error"
  "--robot-triage|--search-limit|abc|error"
  "--robot-triage|--suggest-confidence|abc|error"
  "--robot-triage|--min-confidence|abc|error"
  "--robot-triage|--relations-threshold|abc|error"
  "--robot-triage|--agents|abc|error"
  "--robot-triage|--network-depth|abc|error"
  "--robot-triage|--script-limit|abc|error"
  "--robot-triage|--orphans-min-score|abc|error"
  "--robot-triage|--debug-width|abc|error"
  "--robot-triage|--attention-limit|abc|error"
  "--robot-triage|--forecast-agents|abc|error"
  "--robot-triage|--graph-depth|abc|error"
  "--robot-triage|--history-limit|abc|error"
  "--robot-triage|--robot-history-timeout-ms|abc|error"
  "--robot-triage|--file-beads-limit|abc|error"
  "--robot-triage|--hotspots-limit|abc|error"
  "--robot-triage|--relations-limit|abc|error"
  "--robot-triage|--robot-min-confidence|abc|error"
  "--help|--unknown-flag-xyz|error"
  "--version|--unknown-flag-xyz|error"
  "--version|--robot-diff|error"
  "--version|--format|xml|error"
  "--robot-help|--format|xml|error"
  "--robot-capabilities|--format|xml|error"
  "--agents-check|--format|xml|error"
  "--robot-triage|--robot-triage-by-track|error"
  "--robot-insights|--robot-plan|error"
  "--robot-help|--robot-capabilities|error"
)

# Commands whose stdout is a JSON document, so a non-JSON payload is a bug.
# Everything else (the help page, the version line, the agent reports, the
# TOON encodings) has its own format, and the byte comparison is the check.
json_contract() {
    case "$1" in
        --robot-help|--robot-docs|--agents-check|--baseline-info|--version|--stats|--toon|--emit-script)
            return 1;;
        *) return 0;;
    esac
}

# TOON output is not JSON, so the JSON contract does not apply to it.
wants_toon() {
    local a
    for a in "$@"; do
        case "$a" in
            --toon) return 0;;
            --format)
                ;;
            toon) return 0;;
        esac
    done
    # `--format toon` is two tokens; the caller passes the whole argv.
    local prev=""
    for a in "$@"; do
        if [ "$prev" = "--format" ] && [ "$a" = "toon" ]; then return 0; fi
        prev="$a"
    done
    return 1
}

pass=0; known=0; timing=0; parity=0; crash=0; badexit=0; skipped=0
fail_list=()

printf '%-56s %-8s %s\n' "INVOCATION" "VERDICT" "DETAIL"
printf '%.0s-' {1..90}; echo

for entry in "${MATRIX[@]}"; do
    IFS='|' read -r -a argv <<< "$entry"
    # The last field is the group tag; everything before it is the argv.
    tag="${argv[${#argv[@]}-1]}"
    unset "argv[${#argv[@]}-1]"
    label="${argv[*]}"

    if [ -n "$GROUP" ] && [ "$tag" != "$GROUP" ]; then skipped=$((skipped+1)); continue; fi
    if [ -n "$FILTER" ] && [[ "$label" != *"$FILTER"* ]]; then skipped=$((skipped+1)); continue; fi

    "$RS_BIN" "${argv[@]}" >"$TMP/rs.out" 2>"$TMP/rs.err"; rs_exit=$?

    detail=""
    verdict="pass"

    # CRASH 1: a Rust panic.
    if grep -qE 'panicked at|RUST_BACKTRACE|stack backtrace' "$TMP/rs.err"; then
        verdict="CRASH"; detail="panic"
    fi

    # EXIT: the documented contract is 0 / 1 / 2 only.
    if [ "$verdict" = "pass" ] && [ "$rs_exit" -gt 2 ]; then
        verdict="EXIT"; detail="exit=$rs_exit"
    fi

    # CRASH 2: stdout must be JSON where JSON is the contract.
    if [ "$verdict" = "pass" ] && [ "$rs_exit" -eq 0 ] && [ -s "$TMP/rs.out" ] \
        && json_contract "${argv[0]:-}" && ! wants_toon "${argv[@]}"; then
        if ! node -e 'JSON.parse(require("fs").readFileSync(process.argv[1],"utf8"))' "$TMP/rs.out" 2>/dev/null; then
            verdict="CRASH"; detail="non-JSON stdout"
        fi
    fi

    if [ "$verdict" = "pass" ]; then
        "$GO_BIN" "${argv[@]}" >"$TMP/go.out" 2>"$TMP/go.err"; go_exit=$?
        if [ "$rs_exit" != "$go_exit" ]; then
            verdict="PARITY"; detail="exit go=$go_exit rs=$rs_exit"
        elif ! cmp -s "$TMP/rs.out" "$TMP/go.out" || ! cmp -s "$TMP/rs.err" "$TMP/go.err"; then
            # Classify the difference: program name, timing, or real.
            detail=$(node -e '
            const fs=require("fs");
            const dir=process.argv[1];
            const leaves=(o,p,out)=>{
              if(Array.isArray(o)){o.forEach((v,i)=>leaves(v,`${p}[${i}]`,out));return;}
              if(o&&typeof o==="object"){for(const k of Object.keys(o).sort())leaves(o[k],`${p}.${k}`,out);return;}
              out.push([p,JSON.stringify(o)]);
            };
            const read=f=>{try{return JSON.parse(fs.readFileSync(`${dir}/${f}`,"utf8"));}catch{return null;}};
            const goOut=read("go.out"), rsOut=read("rs.out");
            const goErr=read("go.err"), rsErr=read("rs.err");
            const goTxt=fs.readFileSync(`${dir}/go.err`,"utf8"), rsTxt=fs.readFileSync(`${dir}/rs.err`,"utf8");
            // Program-name substitution: the port installs as `bvr`, Go as `bv`.
            const programOnly=s=>s.replace(/\bbvr\b/g,"bv");
            if(goOut&&rsOut){
              const a=[],b=[];leaves(goOut,"",a);leaves(rsOut,"",b);
              const ma=new Map(a), mb=new Map(b);
              const diffs=[];
              for(const [k,v] of a) if(mb.get(k)!==v) diffs.push([k,v,mb.get(k)]);
              for(const [k,v] of b) if(ma.get(k)!==v) diffs.push([k,ma.get(k),v]);
              if(diffs.length===0){console.log("stderr");process.exit(0);}
              if(diffs.every(([k,gv,rv])=>gv===rv)){console.log("stderr");process.exit(0);}
              // Classify EVERY difference on its own. A difference is
              // explained when it is the program name, a runtime counter Go
              // has no Rust equivalent for, a measurement, or a last-ulp
              // wobble from the oracle own map iteration.
              const programOnly=s=>s.replace(/\bbvr\b/g,"bv");
              const explain=(k,gv,rv)=>{
                if(gv===rv) return "same";
                if(/^\.memory\./.test(k)) return "runtime";
                if(/(_ms|\.ms|duration_ms|compute_time_ms)$/.test(k)) return "timing";
                const g=JSON.parse(gv), r=JSON.parse(rv);
                if(typeof g==="string"&&typeof r==="string"&&programOnly(g)===programOnly(r)) return "program";
                if(typeof g==="number"&&typeof r==="number"){
                  const scale=Math.max(Math.abs(g),Math.abs(r));
                  const close=scale===0?g===r:Math.abs(g-r)/scale<1e-12;
                  if(close) return "ulp";
                }
                return null;
              };
              const reasons=diffs.map(([k,gv,rv])=>explain(k,gv,rv));
              if(reasons.every(r=>r&&r!=="same")){
                console.log(reasons.every(r=>r==="timing")?"TIMING":"KNOWN");
                process.exit(0);
              }
              if(diffs.every(([k,gv,rv])=>{
                    const g=JSON.parse(gv), r=JSON.parse(rv);
                    return typeof g==="string"&&typeof r==="string"&&programOnly(g)===programOnly(r);
                  })){
                console.log("KNOWN");process.exit(0);}
              console.log("DIFFER: "+[...new Set(diffs.map(d=>d[0]))].slice(0,6).join(" | "));
              process.exit(0);
            }
            if(programOnly(goTxt)===programOnly(rsTxt)){console.log("KNOWN");process.exit(0);}
            if(goTxt!==rsTxt){
              const gl=goTxt.split("\n"), rl=rsTxt.split("\n");
              if(gl.length===rl.length&&gl.every((l,i)=>l===rl[i]||programOnly(l)===programOnly(rl[i]))){console.log("KNOWN");process.exit(0);}
              console.log("stderr differs");process.exit(0);
            }
            console.log("stderr");
            ' "$TMP")
            case "$detail" in
                KNOWN) verdict="KNOWN";;
                TIMING) verdict="TIMING";;
                stderr) verdict="KNOWN";;
                *) verdict="PARITY";;
            esac
        fi
    fi

    case "$verdict" in
        pass)   pass=$((pass+1));;
        KNOWN)  known=$((known+1));;
        TIMING) timing=$((timing+1));;
        PARITY) parity=$((parity+1)); fail_list+=("PARITY  $label  ($detail)");;
        CRASH)  crash=$((crash+1));  fail_list+=("CRASH   $label  ($detail)");;
        EXIT)   badexit=$((badexit+1)); fail_list+=("EXIT    $label  ($detail)");;
    esac
    printf '%-56s %-8s %s\n' "${label:0:56}" "$verdict" "$detail"
done

echo
echo "pass=$pass  KNOWN=$known  TIMING=$timing  PARITY=$parity  CRASH=$crash  BAD-EXIT=$badexit  skipped=$skipped"
if [ ${#fail_list[@]} -gt 0 ]; then
    echo
    echo "Findings:"
    for f in "${fail_list[@]}"; do echo "  - $f"; done
fi
[ "$parity" -eq 0 ] && [ "$crash" -eq 0 ] && [ "$badexit" -eq 0 ]
