# Provenance — copied assets

The upstream project, [`beads_viewer`](https://github.com/Dicklesworthstone/beads_viewer),
was written by [@Dicklesworthstone](https://github.com/Dicklesworthstone)
(Jeffrey Emanuel). This repository is an independent Rust port of their tool and
carries their licence (MIT with the OpenAI/Anthropic Rider) over verbatim — see
[LICENSE](LICENSE) and the Credits section in [README.md](README.md).

- `crates/bv-graph-wasm/` = verbatim copy of `beads_viewer/bv-graph-wasm/` from
  Dicklesworthstone/beads_viewer @ `9ace029f1b141c4843a1fbd2c4a365888ef734a5`
  (v0.20.0). No modifications yet. Verified `diff -r` identical.
  **Stale:** the reference clone has since moved to `18afafa` (v0.25.0). This
  vendored copy still reflects v0.20.0 and has not been re-diffed against the
  new parity target.
  Purpose: seed for the future shared graph core (plan §4.3 / Phase 2).
  Extraction into `bv-graph-core` + wasm thin wrapper happens in Phase 0/2,
  NOT now.
